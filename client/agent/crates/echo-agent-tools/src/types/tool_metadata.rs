//! `ToolMetadata` — echo-agent-tools-specific metadata for tools.
//!
//! Each tool implements two traits:
//! 1. `echo_agent_tool_runtime::Tool` — typed Args/Output, `run()` with actual logic
//! 2. `ToolMetadata` — kind, namespace, description template, and optional
//!    overrides for fingerprinting, reminders, etc.
//!
//! Only three methods are required (`kind`, `tool_namespace`,
//! `description_template`); all others have sensible defaults derived
//! from `kind()`.
//!
//! ## Context helpers
//!
//! Tools access session state through `echo_agent_tool_runtime::ToolCallContext`
//! extensions. This module provides helper functions to extract
//! `SharedResources`, resolve the working directory, and read the
//! behavior version.

use std::path::PathBuf;

use crate::types::definition::ToolDefinition;
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::SharedResources;
use crate::types::template_renderer::TemplateRenderer;
use crate::types::tool::{ToolKind, ToolNamespace};

/// EchoAgent-tools-specific metadata trait.
///
/// Each tool struct implements this alongside `echo_agent_tool_runtime::Tool`.
/// Only `kind()`, `namespace()`, and `description_template()` are required;
/// all other methods have defaults.
///
/// The `ToolRegistry` stores a type-erased handle to each tool's
/// `ToolMetadata` impl so it can call `versioned_definition()`, etc.
/// after dispatch.
pub trait ToolMetadata: Send + Sync {
    /// High-level category (Read, Edit, Search, Execute, ...).
    /// Drives template rendering (`${{ tools.by_kind.search }}`) and the
    /// default `is_read_only()` derivation.
    fn kind(&self) -> ToolKind;

    /// Namespace grouping (EchoAgentBuild, Cursor, OpenCode, ...).
    /// Used to build the fully-qualified tool ID at registration time
    /// (e.g., `"EchoAgentBuild:grep"`).
    fn tool_namespace(&self) -> ToolNamespace;

    /// Raw MiniJinja description template with `${{ tools.by_kind.X }}` and
    /// `${{ params.tool.param }}` placeholders. Resolved at finalize time by
    /// the `TemplateRenderer`.
    fn description_template(&self) -> &str;

    // -----------------------------------------------------------------------
    // Defaults — override only when needed
    // -----------------------------------------------------------------------

    /// Whether the tool is read-only (no filesystem / external side-effects).
    /// Default: derived from `kind()`.
    fn is_read_only(&self) -> bool {
        self.kind().is_read_only()
    }

    /// Notification variant tags this tool may emit during execution.
    /// Default: none. Tags match `ToolNotification`'s serde `type` discriminator
    /// (the keys of [`notification_schema_catalog`](crate::notification::notification_schema_catalog)).
    fn emitted_notifications(&self) -> &'static [&'static str] {
        &[]
    }

    /// Requirements expression evaluated at finalize time.
    /// Default: `Expr::True` (no requirements).
    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }

    /// Model-safe fallback description for `echo_agent_tool_runtime::Tool::description()`
    /// implementations: the raw template with all `${{ … }}` / `${% … %}`
    /// markers stripped.
    ///
    /// The registry path (`versioned_definition`) renders templates properly
    /// with the finalized toolset context; this is only for consumers that
    /// bypass the registry, which must never see raw template syntax.
    fn sanitized_description_template(&self) -> String {
        crate::types::template_renderer::strip_template_markers(self.description_template())
    }

    /// Build the tool definition for a given contract version.
    ///
    /// Default: renders `description_template()` via the `TemplateRenderer`
    /// and remaps schema parameter names. Override for tools that need
    /// params-aware descriptions or schemas (e.g., BashTool removes
    /// `is_background` when disabled).
    fn versioned_definition(
        &self,
        _contract_version: Option<&str>,
        client_name: &str,
        description_override: Option<&str>,
        renderer: &TemplateRenderer,
        param_map: &std::collections::HashMap<String, String>,
        input_schema: &serde_json::Value,
        _effective_params: &serde_json::Value,
    ) -> ToolDefinition {
        let raw_desc = description_override.unwrap_or_else(|| self.description_template());
        let description = renderer.render(raw_desc).unwrap_or_else(|e| {
            crate::types::template_renderer::strip_markers_on_render_failure(raw_desc, &e)
        });
        let remapped_schema = if param_map.is_empty() {
            input_schema.clone()
        } else {
            crate::util::remap::remap_schema_properties(input_schema, param_map)
        };
        ToolDefinition::function(client_name, Some(&description), remapped_schema)
    }
}

/// Extract `SharedResources` from the runtime tool-call context.
///
/// `ToolBridge` inserts `SharedResources` into `ctx.extensions` before
/// dispatching through the `LocalRegistry`.
pub fn shared_resources(
    ctx: &echo_agent_tool_runtime::ToolCallContext,
) -> Result<SharedResources, echo_agent_tool_runtime::ToolError> {
    ctx.extensions
        .get::<SharedResources>()
        .map(|arc| (*arc).clone())
        .ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::custom(
                "missing_resources",
                "SharedResources not available in ToolCallContext extensions",
            )
        })
}

/// Resolve the working directory from the runtime context.
///
/// Checks `Cwd` extension first (set when the caller provides a per-call
/// override), then falls back to `Cwd` in `SharedResources`.
pub async fn resolve_cwd(
    ctx: &echo_agent_tool_runtime::ToolCallContext,
    resources: &SharedResources,
) -> Result<PathBuf, echo_agent_tool_runtime::ToolError> {
    if let Some(cwd) = ctx.extensions.get::<echo_agent_tool_runtime::Cwd>() {
        return Ok(cwd.0.clone());
    }
    let res = resources.lock().await;
    res.get::<crate::types::resources::Cwd>()
        .map(|c| c.0.clone())
        .ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::custom(
                "missing_cwd",
                "Cwd not available in Resources",
            )
        })
}

/// Build a `ToolCallContext` with `SharedResources` installed and a fresh
/// v7 call id.
///
/// Convenience for tests — replaces the per-tool `make_ctx` / `runtime_ctx`
/// helpers that were duplicated across ~50 tool implementations. Use
/// [`test_ctx_with_call_id`] when the test needs a specific call id.
pub fn test_ctx(resources: SharedResources) -> echo_agent_tool_runtime::ToolCallContext {
    let mut ctx = echo_agent_tool_runtime::ToolCallContext::default();
    ctx.extensions.insert(resources);
    // Default streaming gate ON so existing tests exercise the stream path.
    ctx.extensions
        .insert(echo_agent_tool_runtime::WorkspaceViewerContext {
            stream_tool_progress: true,
        });
    ctx
}

/// Like [`test_ctx`] but with a caller-specified call id.
///
/// Falls back to a fresh v7 id if `call_id` is not a valid `ToolCallId`.
pub fn test_ctx_with_call_id(
    resources: SharedResources,
    call_id: &str,
) -> echo_agent_tool_runtime::ToolCallContext {
    let id = echo_agent_tool_protocol::ToolCallId::new(call_id)
        .unwrap_or_else(|_| echo_agent_tool_protocol::ToolCallId::new_v7());
    let mut ctx = echo_agent_tool_runtime::ToolCallContext::new(id);
    ctx.extensions.insert(resources);
    ctx.extensions
        .insert(echo_agent_tool_runtime::WorkspaceViewerContext {
            stream_tool_progress: true,
        });
    ctx
}

/// Read the behavior version from the runtime context, if set.
pub fn behavior_version(ctx: &echo_agent_tool_runtime::ToolCallContext) -> Option<String> {
    ctx.extensions
        .get::<echo_agent_tool_runtime::BehaviorVersion>()
        .map(|v| v.0.clone())
}

/// This tool's own canonical→client param-name map, stamped on the dispatch
/// context by `prepare_dispatch` / `call_raw`. Returns an empty (identity)
/// map when absent — e.g. unit tests that call `Tool::run` directly — so
/// callers resolve to canonical names. Prefer this over kind-wide
/// [`crate::types::template_renderer::TemplateRenderer::param_for_kind`] when
/// naming *this* tool's own params (a sibling tool sharing the `ToolKind`
/// can rename the same field differently).
pub fn invoking_param_names(
    ctx: &echo_agent_tool_runtime::ToolCallContext,
) -> crate::types::resources::InvokingToolParamNames {
    ctx.extensions
        .get::<crate::types::resources::InvokingToolParamNames>()
        .map(|arc| (*arc).clone())
        .unwrap_or_default()
}
