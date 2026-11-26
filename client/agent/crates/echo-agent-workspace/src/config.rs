//! Workspace and session configuration types.
use crate::capability::CapabilityMode;
use crate::hub::HubConfig;
use echo_agent_tools::registry::types::{SessionContext, ToolRegistryBuilder, ToolServerConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
/// Default capacity for the workspace event broadcast channel.
pub const DEFAULT_EVENT_BUFFER_CAPACITY: usize = 64;
/// A session-lifetime terminal backend paired with its explicit shutdown hook.
///
/// The backend (background-task registry + persistent shell) is owned by the
/// [`WorkspaceSession`](crate::session::WorkspaceSession) and injected into
/// every toolset re-resolve for that session, so background tasks and shell
/// state survive toolset swaps. The shutdown hook fires the backend's cancel
/// token — killing every child process group and stopping the actor — so
/// `drop_session`/evict teardown is an explicit act rather than a side effect
/// of the last `Arc` drop.
#[derive(Clone)]
pub struct SessionTerminalBackend {
    backend: Arc<dyn echo_agent_tools::computer::types::TerminalBackend>,
    shutdown: Arc<dyn Fn() + Send + Sync>,
}
impl SessionTerminalBackend {
    /// Pair an already-erased `backend` with its shutdown hook.
    ///
    /// Extension point for [`SessionContextFactory`] implementors whose
    /// backend is not a `LocalTerminalBackend` (the fields are private, so
    /// this is the only way to satisfy `build_terminal_backend` for other
    /// backend types); in-repo factories use [`Self::local`].
    pub fn new(
        backend: Arc<dyn echo_agent_tools::computer::types::TerminalBackend>,
        shutdown: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self { backend, shutdown }
    }
    /// Wrap a [`LocalTerminalBackend`], wiring the shutdown hook to its
    /// cancel token.
    ///
    /// [`LocalTerminalBackend`]: echo_agent_tools::computer::local::LocalTerminalBackend
    pub fn local(backend: echo_agent_tools::computer::local::LocalTerminalBackend) -> Self {
        let canceller = backend.clone();
        Self {
            backend: Arc::new(backend),
            shutdown: Arc::new(move || canceller.cancel()),
        }
    }
    /// The type-erased backend, as injected into toolset resolves.
    pub fn backend(&self) -> &Arc<dyn echo_agent_tools::computer::types::TerminalBackend> {
        &self.backend
    }
    /// Explicitly shut the backend down: kills all of its child process
    /// groups and stops its actor.
    pub fn shutdown(&self) {
        (self.shutdown)();
    }
}
impl std::fmt::Debug for SessionTerminalBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTerminalBackend")
            .finish_non_exhaustive()
    }
}
/// Pluggable producer of [`SessionContext`] / [`ToolRegistryBuilder`]
/// for each session.
///
/// The workspace itself doesn't know how to construct the tool runtime
/// (terminal backend, file system, persistence path, MCP client config,
/// notification handle, ...) -- those come from the embedder (TUI, SDK,
/// or remote sampler). The embedder hands us a factory at
/// `WorkspaceHandle::new` time and we call it on every session
/// resolution.
pub trait SessionContextFactory: Send + Sync {
    /// Build a fresh [`SessionContext`] for the given session, around the
    /// given terminal `backend` (constructing one here would waste an actor
    /// per resolve — the pipeline rebuilds toolsets around the session-owned
    /// backend, so the caller always supplies it).
    fn build_session_context(
        &self,
        session_id: &str,
        cwd: PathBuf,
        session_env: Arc<HashMap<String, String>>,
        backend: Arc<dyn echo_agent_tools::computer::types::TerminalBackend>,
    ) -> SessionContext;
    /// Build the session-lifetime terminal backend for a new session.
    /// Called once per session create/fork; toolset re-resolves reuse the
    /// session's stored backend instead of building another.
    fn build_terminal_backend(&self) -> SessionTerminalBackend;
    /// Build a fresh [`ToolRegistryBuilder`] with the workspace's
    /// full set of registered tools.
    fn registry_builder(&self) -> ToolRegistryBuilder;
    fn known_tool_ids(&self) -> Arc<std::collections::HashSet<String>> {
        Arc::new(self.registry_builder().known_tool_ids())
    }
}
/// Placeholder for the cross-session memory backend config.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct MemoryConfig {}
/// Per-session toolset/capability selection from the `session.bind`
/// metadata. Absent fields fall back to the workspace default and `CapabilityMode::All`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WorkspaceBindConfig {
    /// Named toolset preset from the wire. **Never resolved** (see
    /// [`Self::resolve`]); parsed only so it can be logged.
    pub preset: Option<String>,
    /// Capability mode applied to the session's toolset.
    pub capability_mode: Option<CapabilityMode>,
    /// Fully-specified toolset in the runtime serde shape. Takes precedence
    /// over `tools`.
    pub tool_config: Option<ToolServerConfig>,
    /// Per-user feature-flag bag. `None` on legacy payloads → tools
    /// fall back to their safe defaults.
    pub viewer_ctx: Option<echo_agent_tool_runtime::WorkspaceViewerContext>,
    /// Initial auto-approve (YOLO) state. `None` on legacy payloads →
    /// fail-closed (false).
    pub yolo_mode: Option<bool>,
    /// Plane-configured toolset in the gRPC wire shape. An empty list is
    /// treated as unset (proto3 repeated default).
    pub tools: Option<Vec<echo_agent_tools_api::ToolConfigEntry>>,
    pub manifest_version: Option<String>,
    pub manifest_hash: Option<String>,
    /// Opt-in: forward `BackgroundTaskCompleted` system notifications for this session.
    pub system_notifications: bool,
    pub rpc_only: bool,
    /// Real guest session root (`/workspace/<conversation_id>`). When set,
    /// the workspace virtualizes that tree as `/workspace`.
    pub session_root: Option<PathBuf>,
}
/// Outcome of resolving a [`WorkspaceBindConfig`]; lets callers fail closed
/// instead of widening to the default toolset. Deliberately has **no preset
/// arm** (see [`WorkspaceBindConfig::resolve`]).
#[derive(Debug)]
pub enum ResolvedToolset {
    /// An explicit toolset (`tool_config` or `tools`).
    Toolset(ResolvedTools),
    /// No explicit toolset was specified and the workspace allows falling
    /// back to its default catalog (local/CLI embedders only).
    UseDefault,
    /// No explicit toolset was specified and the workspace requires one
    /// (sandbox-launched standalone servers) — fail closed.
    MissingToolConfig,
    /// `tools` entries were specified but at least one failed to convert.
    InvalidToolConfig(echo_agent_tools::registry::proto_convert::ToolConfigEntryError),
}
/// A resolved toolset plus the pinned entries this binary could not serve.
#[derive(Debug)]
pub struct ResolvedTools {
    pub toolset: ToolServerConfig,
    /// Pinned `tools` ids unknown to this binary's registry, sorted. Always
    /// empty for `tool_config` resolutions.
    pub unserved_tool_ids: Vec<String>,
}
impl ResolvedTools {
    /// A fully-served toolset (no divergence).
    fn full(toolset: ToolServerConfig) -> Self {
        Self {
            toolset,
            unserved_tool_ids: Vec::new(),
        }
    }
}
impl WorkspaceBindConfig {
    /// Parse hub `session.bind` metadata. The envelope is the shared
    /// [`echo_agent_tool_runtime::WorkspaceBindMetadata`] (same type the emitter
    /// serializes); `tool_config` is a consumer-only raw escape hatch read
    /// separately.
    pub fn from_metadata(metadata: &serde_json::Value) -> Self {
        let wire: echo_agent_tool_runtime::WorkspaceBindMetadata =
            serde_json::from_value(metadata.clone()).unwrap_or_default();
        Self {
            preset: wire.preset,
            capability_mode: wire
                .capability_mode
                .and_then(|s| serde_json::from_value(serde_json::Value::String(s)).ok()),
            tool_config: metadata
                .as_object()
                .and_then(|obj| obj.get("tool_config"))
                .and_then(|v| parse_field("tool_config", v)),
            viewer_ctx: wire.viewer_ctx,
            yolo_mode: wire.yolo_mode,
            tools: Some(wire.tools).filter(|tools| !tools.is_empty()),
            manifest_version: wire.manifest_version,
            manifest_hash: wire.manifest_hash,
            system_notifications: wire.system_notifications.unwrap_or(false),
            rpc_only: wire.rpc_only,
            session_root: wire.session_root.map(PathBuf::from),
        }
    }
    /// Resolve the selected toolset.
    ///
    /// Precedence: `tool_config` > `tools` (wire entries) > default/fail-closed.
    /// Pinned `tools` are served per entry: ids `known_id` rejects are dropped
    /// and reported in [`ResolvedTools::unserved_tool_ids`] instead of
    /// silently falling back to a different toolset.
    ///
    /// **Presets are never resolved** — a `preset` on the wire is logged and
    /// ignored; only explicit `tools`/`tool_config` may select a toolset.
    ///
    /// With `require_explicit_toolset` (sandbox standalone servers) a bind
    /// without an explicit toolset fails closed instead of widening to the
    /// binary's default catalog.
    pub fn resolve(
        &self,
        known_id: &dyn Fn(&str) -> bool,
        require_explicit_toolset: bool,
    ) -> ResolvedToolset {
        if let Some(cfg) = &self.tool_config {
            for (idx, tool) in cfg.tools.iter().enumerate() {
                if let Err(err) = echo_agent_tools_api::config_validation::validate_name_override(
                    idx,
                    &tool.id,
                    tool.name_override.as_deref(),
                ) {
                    return ResolvedToolset::InvalidToolConfig(err);
                }
            }
            return ResolvedToolset::Toolset(ResolvedTools::full(cfg.clone()));
        }
        if let Some(tools) = &self.tools {
            let mut unserved_tool_ids: Vec<String> = Vec::new();
            let mut served = Vec::with_capacity(tools.len());
            for (idx, entry) in tools.iter().enumerate() {
                if !known_id(&entry.id) {
                    unserved_tool_ids.push(entry.id.clone());
                    continue;
                }
                match echo_agent_tools::registry::proto_convert::tool_config_from_entry(
                    idx,
                    entry.clone(),
                ) {
                    Ok(tc) => served.push(tc),
                    Err(err) => return ResolvedToolset::InvalidToolConfig(err),
                }
            }
            unserved_tool_ids.sort_unstable();
            if !unserved_tool_ids.is_empty() {
                tracing::warn!(
                    unserved = ?unserved_tool_ids,
                    config_manifest_version = ?self.manifest_version,
                    running_version = echo_agent_version::VERSION,
                    "session.bind: serving known subset of pinned tools"
                );
            }
            return ResolvedToolset::Toolset(ResolvedTools {
                toolset: ToolServerConfig {
                    tools: served,
                    behavior_preset: None,
                },
                unserved_tool_ids,
            });
        }
        if let Some(preset) = self.preset.as_deref() {
            tracing::warn!(
                preset,
                "session.bind: toolset presets are not resolved by the workspace \
                 server; pass an explicit `tools` config"
            );
        }
        if require_explicit_toolset {
            ResolvedToolset::MissingToolConfig
        } else {
            ResolvedToolset::UseDefault
        }
    }
}
/// Parse a single bind-metadata field, ignoring (and logging) a malformed value.
fn parse_field<T: serde::de::DeserializeOwned>(name: &str, value: &serde_json::Value) -> Option<T> {
    match serde_json::from_value(value.clone()) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            tracing::warn!(
                field = name,
                error = %e,
                "session.bind metadata: ignoring malformed field"
            );
            None
        }
    }
}
#[cfg(test)]
mod bind_config_tests {
    use super::*;
    /// Predicate for tests where every pinned id is known to the binary.
    fn all_known(_: &str) -> bool {
        true
    }
    /// Predicate for tests simulating a binary that knows none of the ids.
    fn none_known(_: &str) -> bool {
        false
    }
    #[test]
    fn parses_preset_and_capability() {
        let v = serde_json::json!({"preset": "explore", "capability_mode": "read_only"});
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert_eq!(cfg.preset.as_deref(), Some("explore"));
        assert_eq!(
            cfg.capability_mode,
            Some(crate::capability::CapabilityMode::ReadOnly)
        );
    }
    #[test]
    fn defaults_on_empty_or_mismatched_metadata() {
        let empty = WorkspaceBindConfig::from_metadata(&serde_json::json!({}));
        assert!(empty.preset.is_none());
        assert!(empty.capability_mode.is_none());
        assert!(matches!(
            empty.resolve(&all_known, false),
            ResolvedToolset::UseDefault
        ));
        let weird = WorkspaceBindConfig::from_metadata(&serde_json::json!("hello"));
        assert!(matches!(
            weird.resolve(&all_known, false),
            ResolvedToolset::UseDefault
        ));
    }
    /// Presets are banned: any preset (known or not) is ignored — never
    /// resolved to a toolset, and never widened to the default in strict mode.
    #[test]
    fn presets_are_never_resolved() {
        for preset in ["explore", "echo-agent-computer", "bogus"] {
            let cfg = WorkspaceBindConfig::from_metadata(&serde_json::json!({ "preset": preset }));
            assert!(
                matches!(cfg.resolve(&all_known, false), ResolvedToolset::UseDefault),
                "lax mode must fall through to the default, preset={preset}"
            );
            assert!(
                matches!(
                    cfg.resolve(&all_known, true),
                    ResolvedToolset::MissingToolConfig
                ),
                "strict mode must fail closed, preset={preset}"
            );
        }
    }
    /// Strict mode (sandbox standalone server): no explicit toolset on the
    /// bind ⇒ fail closed instead of widening to the default catalog.
    #[test]
    fn strict_mode_requires_explicit_toolset() {
        let empty = WorkspaceBindConfig::from_metadata(&serde_json::json!({}));
        assert!(matches!(
            empty.resolve(&all_known, true),
            ResolvedToolset::MissingToolConfig
        ));
    }
    #[test]
    fn malformed_field_does_not_discard_valid_siblings() {
        let v = serde_json::json!({"preset": "explore", "capability_mode": "raed_only"});
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert_eq!(cfg.preset.as_deref(), Some("explore"));
        assert!(cfg.capability_mode.is_none());
    }
    #[test]
    fn workspace_bind_config_from_metadata_extracts_viewer_ctx() {
        let v = serde_json::json!({
            "preset": "explore",
            "viewer_ctx": {"stream_tool_progress": true},
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert_eq!(cfg.preset.as_deref(), Some("explore"));
        let viewer = cfg.viewer_ctx.expect("viewer_ctx parsed");
        assert!(viewer.stream_tool_progress);
    }
    /// Legacy payload without `viewer_ctx` still parses (mixed-version
    /// proxy/workspace deploys).
    #[test]
    fn workspace_bind_config_from_metadata_legacy_omitted_viewer_ctx() {
        let v = serde_json::json!({"preset": "explore"});
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert!(cfg.viewer_ctx.is_none());
    }
    #[test]
    fn workspace_bind_config_from_metadata_extracts_yolo_mode() {
        let v = serde_json::json!({"preset": "explore", "yolo_mode": true});
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert_eq!(cfg.yolo_mode, Some(true));
    }
    #[test]
    fn workspace_bind_config_yolo_mode_omitted_or_malformed_fails_closed() {
        let omitted = WorkspaceBindConfig::from_metadata(&serde_json::json!({"preset": "explore"}));
        assert!(omitted.yolo_mode.is_none());
        let malformed = WorkspaceBindConfig::from_metadata(
            &serde_json::json!({"preset": "explore", "yolo_mode": "yes"}),
        );
        assert!(malformed.yolo_mode.is_none());
        assert_eq!(malformed.preset.as_deref(), Some("explore"));
    }
    #[test]
    fn workspace_bind_config_extracts_system_notifications_flag() {
        let on =
            WorkspaceBindConfig::from_metadata(&serde_json::json!({"system_notifications": true}));
        assert!(on.system_notifications);
        let off = WorkspaceBindConfig::from_metadata(&serde_json::json!({"preset": "explore"}));
        assert!(!off.system_notifications);
        let explicit_off =
            WorkspaceBindConfig::from_metadata(&serde_json::json!({"system_notifications": false}));
        assert!(!explicit_off.system_notifications);
    }
    #[test]
    fn workspace_bind_config_extracts_rpc_only_flag() {
        let on = WorkspaceBindConfig::from_metadata(&serde_json::json!({"rpc_only": true}));
        assert!(on.rpc_only);
        let off = WorkspaceBindConfig::from_metadata(&serde_json::json!({"preset": "explore"}));
        assert!(!off.rpc_only);
        let explicit_off =
            WorkspaceBindConfig::from_metadata(&serde_json::json!({"rpc_only": false}));
        assert!(!explicit_off.rpc_only);
    }
    #[test]
    fn workspace_bind_config_extracts_session_root() {
        let on = WorkspaceBindConfig::from_metadata(&serde_json::json!({
            "session_root": "/workspace/conv-abc",
        }));
        assert_eq!(
            on.session_root.as_deref(),
            Some(std::path::Path::new("/workspace/conv-abc"))
        );
        let off = WorkspaceBindConfig::from_metadata(&serde_json::json!({"preset": "explore"}));
        assert!(off.session_root.is_none());
        let malformed = WorkspaceBindConfig::from_metadata(&serde_json::json!({
            "session_root": 123,
            "preset": "explore",
        }));
        assert!(malformed.session_root.is_none());
        assert_eq!(malformed.preset.as_deref(), Some("explore"));
    }
    #[test]
    fn workspace_bind_config_from_metadata_extracts_manifest_fields() {
        let v = serde_json::json!({
            "preset": "explore",
            "manifest_version": "v1",
            "manifest_hash": "abc123",
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert_eq!(cfg.manifest_version.as_deref(), Some("v1"));
        assert_eq!(cfg.manifest_hash.as_deref(), Some("abc123"));
    }
    #[test]
    fn workspace_bind_config_manifest_fields_default_to_none_when_absent() {
        let cfg = WorkspaceBindConfig::from_metadata(&serde_json::json!({"preset": "explore"}));
        assert!(cfg.manifest_version.is_none());
        assert!(cfg.manifest_hash.is_none());
    }
    /// Consumer-side parity test for the bind-metadata `tools` contract;
    /// pairs with the producer-side pin test in agentic-sampler's
    /// `configs::plane` tests.
    #[test]
    fn tools_entries_resolve_to_tool_server_config() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [
                {
                    "id": "EchoAgentBuild:grep",
                    "params_json": "{\"max_results\":50}",
                    "name_override": "search",
                    "params_name_overrides": {"pattern": "query"},
                    "behavior_version": "legacy-0.4.10",
                    "description_override": "Search the codebase",
                },
                {"id": "EchoAgentBuild:read_file"},
            ],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, false) else {
            panic!("tools entries must resolve to an explicit toolset");
        };
        assert!(resolved.unserved_tool_ids.is_empty());
        let toolset = resolved.toolset;
        assert_eq!(
            toolset.behavior_preset, None,
            "always the 'current' default"
        );
        assert_eq!(toolset.tools.len(), 2);
        let grep = &toolset.tools[0];
        assert_eq!(grep.id, "EchoAgentBuild:grep");
        assert_eq!(
            grep.params,
            serde_json::json!({"max_results": 50}).as_object().cloned()
        );
        assert_eq!(grep.name_override.as_deref(), Some("search"));
        assert_eq!(
            grep.params_name_overrides.as_ref().unwrap()["pattern"],
            "query"
        );
        assert_eq!(grep.behavior_version.as_deref(), Some("legacy-0.4.10"));
        assert_eq!(
            grep.description_override.as_deref(),
            Some("Search the codebase")
        );
        assert_eq!(grep.kind, None);
        assert_eq!(toolset.tools[1].id, "EchoAgentBuild:read_file");
    }
    #[test]
    fn explicit_tool_config_wins_over_tools_entries() {
        let v = serde_json::json!({
            "tool_config": {"tools": [{"id": "raw:tool"}]},
            "tools": [{"id": "wire:tool"}],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, false) else {
            panic!("must resolve to a toolset");
        };
        assert_eq!(resolved.toolset.tools.len(), 1);
        assert_eq!(resolved.toolset.tools[0].id, "raw:tool");
    }
    #[test]
    fn tools_entries_win_even_with_preset_present() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [{"id": "wire:tool"}],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, false) else {
            panic!("must resolve to a toolset");
        };
        assert_eq!(resolved.toolset.tools.len(), 1);
        assert_eq!(resolved.toolset.tools[0].id, "wire:tool");
    }
    #[test]
    fn empty_tools_array_is_treated_as_unset() {
        let v = serde_json::json!({"preset": "explore", "tools": []});
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert!(cfg.tools.is_none());
        assert!(matches!(
            cfg.resolve(&all_known, false),
            ResolvedToolset::UseDefault
        ));
        assert!(matches!(
            cfg.resolve(&all_known, true),
            ResolvedToolset::MissingToolConfig
        ));
        let no_preset = serde_json::json!({"tools": []});
        let cfg = WorkspaceBindConfig::from_metadata(&no_preset);
        assert!(matches!(
            cfg.resolve(&all_known, false),
            ResolvedToolset::UseDefault
        ));
    }
    #[test]
    fn invalid_tools_entry_fails_closed() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [{"id": "bad:tool", "params_json": "{not json"}],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        match cfg.resolve(&all_known, false) {
            ResolvedToolset::InvalidToolConfig(err) => {
                assert_eq!(err.tool_id, "bad:tool");
                assert_eq!(err.index, 0);
            }
            other => panic!("expected InvalidToolConfig, got {other:?}"),
        }
    }
    #[test]
    fn invalid_name_override_fails_closed() {
        let v = serde_json::json!({
            "tools": [
                {"id": "wire:ok", "name_override": "fine_name"},
                {"id": "wire:bad", "name_override": "not a tool id!"},
            ],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        match cfg.resolve(&all_known, true) {
            ResolvedToolset::InvalidToolConfig(err) => {
                assert_eq!(err.tool_id, "wire:bad");
                assert_eq!(err.field_path(), "tools[1].name_override");
            }
            other => panic!("expected InvalidToolConfig, got {other:?}"),
        }
    }
    #[test]
    fn tool_config_escape_hatch_invalid_name_override_fails_closed() {
        let v = serde_json::json!({
            "tool_config": {"tools": [
                {"id": "raw:ok", "name_override": "fine_name"},
                {"id": "raw:bad", "name_override": "not a tool id!"},
            ]},
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        match cfg.resolve(&all_known, true) {
            ResolvedToolset::InvalidToolConfig(err) => {
                assert_eq!(err.tool_id, "raw:bad");
                assert_eq!(err.field_path(), "tools[1].name_override");
            }
            other => panic!("expected InvalidToolConfig, got {other:?}"),
        }
        let v = serde_json::json!({
            "tool_config": {"tools": [{"id": "raw:ok", "name_override": "fine_name"}]},
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, true) else {
            panic!("valid escape-hatch config must resolve");
        };
        assert_eq!(resolved.toolset.tools.len(), 1);
    }
    #[test]
    fn invalid_entry_error_reports_wire_index_after_unknown_drop() {
        let v = serde_json::json!({
            "tools": [
                {"id": "wire:unknown"},
                {"id": "wire:bad", "params_json": "{not json"},
            ],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let known = |id: &str| id != "wire:unknown";
        match cfg.resolve(&known, false) {
            ResolvedToolset::InvalidToolConfig(err) => {
                assert_eq!(err.tool_id, "wire:bad");
                assert_eq!(
                    err.index, 1,
                    "index must be the wire position, not the known-subset position"
                );
            }
            other => panic!("expected InvalidToolConfig, got {other:?}"),
        }
    }
    #[test]
    fn valid_name_overrides_resolve_intact() {
        let v = serde_json::json!({
            "tools": [
                {"id": "wire:a", "name_override": "renamed_a"},
                {"id": "wire:b"},
            ],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, true) else {
            panic!("well-formed overrides must resolve to a toolset");
        };
        assert_eq!(resolved.toolset.tools.len(), 2);
        assert_eq!(
            resolved.toolset.tools[0].name_override.as_deref(),
            Some("renamed_a")
        );
        assert_eq!(resolved.toolset.tools[1].name_override, None);
    }
    #[test]
    fn pinned_tools_all_known_serves_full_expansion() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [{"id": "wire:tool"}],
            "manifest_version": "9.9.9-any",
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, false) else {
            panic!("known pinned tools must use the tools expansion");
        };
        assert!(resolved.unserved_tool_ids.is_empty());
        assert_eq!(resolved.toolset.tools.len(), 1);
        assert_eq!(resolved.toolset.tools[0].id, "wire:tool");
    }
    /// Unknown ids must be partitioned and reported, never silently replaced
    /// by live preset resolution.
    #[test]
    fn pinned_tools_unknown_ids_are_partitioned_and_reported() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [
                {"id": "wire:known"},
                {"id": "wire:zz_unknown"},
                {"id": "wire:aa_unknown"},
            ],
            "manifest_version": "0.0.0-stale",
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let known = |id: &str| id == "wire:known";
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&known, false) else {
            panic!("partial coverage must still resolve to the known subset");
        };
        assert_eq!(resolved.toolset.tools.len(), 1);
        assert_eq!(resolved.toolset.tools[0].id, "wire:known");
        assert_eq!(
            resolved.unserved_tool_ids,
            vec!["wire:aa_unknown".to_owned(), "wire:zz_unknown".to_owned()],
            "unserved ids are reported sorted"
        );
    }
    /// A fully-unknown expansion serves empty and reports every id — it never
    /// widens to preset/default.
    #[test]
    fn pinned_tools_all_unknown_serves_empty_and_reports_all() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [{"id": "wire:tool"}],
            "manifest_version": "0.0.0-stale",
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&none_known, false) else {
            panic!("all-unknown expansion must resolve (empty), not fall back");
        };
        assert!(resolved.toolset.tools.is_empty());
        assert_eq!(resolved.unserved_tool_ids, vec!["wire:tool".to_owned()]);
    }
    #[test]
    fn legacy_tools_without_manifest_version_are_not_gated() {
        let v = serde_json::json!({
            "preset": "explore",
            "tools": [{"id": "wire:tool"}],
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert!(cfg.manifest_version.is_none());
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&all_known, false) else {
            panic!("legacy unpinned tools must resolve without gating");
        };
        assert_eq!(resolved.toolset.tools.len(), 1);
        assert_eq!(resolved.toolset.tools[0].id, "wire:tool");
    }
    #[test]
    fn tool_config_wins_regardless_of_stale_manifest_version() {
        let v = serde_json::json!({
            "tool_config": {"tools": [{"id": "raw:tool"}]},
            "tools": [{"id": "wire:tool"}],
            "manifest_version": "0.0.0-stale",
        });
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        let ResolvedToolset::Toolset(resolved) = cfg.resolve(&none_known, false) else {
            panic!("tool_config must always win");
        };
        assert!(resolved.unserved_tool_ids.is_empty());
        assert_eq!(resolved.toolset.tools.len(), 1);
        assert_eq!(resolved.toolset.tools[0].id, "raw:tool");
    }
    #[test]
    fn malformed_tools_field_is_dropped_keeping_siblings() {
        let v = serde_json::json!({"preset": "explore", "tools": "not-a-list"});
        let cfg = WorkspaceBindConfig::from_metadata(&v);
        assert!(cfg.tools.is_none());
        assert!(matches!(
            cfg.resolve(&all_known, false),
            ResolvedToolset::UseDefault
        ));
        assert!(matches!(
            cfg.resolve(&all_known, true),
            ResolvedToolset::MissingToolConfig
        ));
    }
}
/// Top-level config required to construct a [`crate::handle::WorkspaceHandle`].
///
/// `#[non_exhaustive]` so future fields are non-breaking.
#[non_exhaustive]
pub struct WorkspaceConfig {
    /// Workspace root directory.
    pub root_cwd: PathBuf,
    /// Baseline tool config for the main session.
    pub default_tool_config: ToolServerConfig,
    /// Whether session-scoped fs operations should respect `.gitignore`.
    pub respect_gitignore: bool,
    /// Optional cross-session memory config.
    pub memory_config: Option<MemoryConfig>,
    /// Capacity of the workspace event broadcast channel.
    pub event_buffer_capacity: usize,
    /// Pluggable [`SessionContext`] / [`ToolRegistryBuilder`] producer.
    pub session_factory: Arc<dyn SessionContextFactory>,
    /// Global hook sources (e.g. `~/.claude/settings.json`, `~/.echo-agent/hooks/`).
    pub hook_global_sources: Vec<HookSourceConfig>,
    /// Project-scoped hook sources (e.g. `<project>/.echo-agent/hooks/`).
    pub hook_project_sources: Vec<HookSourceConfig>,
    /// Skill discovery configuration: additional skill paths and
    /// path-prefix ignore list. Stored on `WorkspaceShared` for
    /// `discover_skills` calls. Defaults to empty (no extra paths,
    /// no ignores).
    pub skills_config: crate::discovery::SkillsConfig,
    /// Plugin discovery configuration: CLI plugin dirs, config paths,
    /// and disabled/enabled lists. Stored on `WorkspaceShared` for
    /// `discover_plugins` calls. Defaults to empty.
    pub plugin_discovery_config: crate::discovery::PluginDiscoveryConfig,
    /// Optional server configuration. When `Some`, the workspace
    /// can connect to the server after construction via
    /// [`WorkspaceHandle::connect_hub`](crate::handle::WorkspaceHandle::connect_hub).
    pub hub_config: Option<HubConfig>,
    /// Auth provider for EchoAgent service calls made from workspace-scoped code.
    /// `None` for workspaces that do not configure service auth.
    pub auth_provider: Option<echo_agent_computer_hub_sdk::SharedAuthProvider>,
    /// Metadata attached to the tool server registration.
    /// Propagated through the server to `ServerInfo.metadata` in
    /// `servers.list` responses so harness clients can identify the
    /// sandbox that started the tool server.
    pub server_metadata: Option<serde_json::Value>,
    /// Runtime-tunable timing/threshold config for the tool server.
    pub status_config: crate::status_config::StatusConfig,
    /// Folder-trust verdict for repo-local (project-scoped) LSP servers from
    /// `<cwd>/.echo-agent/lsp.json`: `false` drops them at load, `true` keeps them. The
    /// shell caller resolves the verdict and threads it in; callers without a
    /// folder-trust decision pass `true`.
    pub project_lsp_trusted: bool,
    /// Fail `session.bind`s without an explicit toolset closed instead of
    /// widening to `default_tool_config`. Set by sandbox-launched standalone
    /// servers; local/CLI embedders keep the default-catalog fallback.
    pub require_explicit_toolset: bool,
    /// Confine `echo.agent/fs/*` / `workspace.fs_*` resolution to the workspace root
    /// (reject `..`, absolute-outside-root, symlink escapes). Default `false`
    /// (unconfined) — set to `true` only by the workspace server on a remote
    /// sandbox, where the root is a real tenant boundary.
    pub confine_fs_to_workspace_root: bool,
}
/// Metadata a tool server announces so hub consumers can identify and route
/// to it. Every field is optional and independently sourced; a local process
/// announces none.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceServerMetadata {
    /// Sandbox that provisioned this server. Absent for local servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_id: Option<String>,
    /// Logical sandbox-service session UUID, from the `ECHO_AGENT_SESSION_ID` env
    /// var. Present whenever that var is set (every sandbox container, start
    /// and restore), absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Provider that provisioned this server. Populated on the start path
    /// only (no container-side source on restore); absent for local servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Per-spawn launch nonce minted by the sandbox orchestrator and echoed
    /// verbatim on the diagnostics `/ready` endpoint. Absent for local/legacy
    /// launches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
}
impl WorkspaceServerMetadata {
    /// Merge an env-sourced logical session id into caller-supplied
    /// tool-server metadata (`None` on the restore/local path).
    ///
    /// `env_session_id` is the raw `ECHO_AGENT_SESSION_ID`; empty is normalized to
    /// absent. An explicit `session_id` already in `metadata` is never
    /// clobbered. A non-object `metadata` value is returned unchanged (a
    /// defensive no-op — the sole caller always sends an object).
    pub fn merge_session_metadata(
        metadata: Option<serde_json::Value>,
        env_session_id: Option<String>,
    ) -> Option<serde_json::Value> {
        let env_session_id = env_session_id.filter(|s| !s.is_empty());
        match metadata {
            Some(mut value) => {
                if let Some(session_id) = env_session_id
                    && let Some(obj) = value.as_object_mut()
                    && !obj.contains_key("session_id")
                {
                    obj.insert(
                        "session_id".to_owned(),
                        serde_json::Value::String(session_id),
                    );
                }
                Some(value)
            }
            None => serde_json::to_value(WorkspaceServerMetadata {
                sandbox_id: None,
                session_id: env_session_id,
                provider_id: None,
                launch_id: None,
            })
            .ok(),
        }
    }
}
impl WorkspaceConfig {
    /// Construct a minimal config suitable for proxy-mode workspaces
    /// where the workspace is used primarily as a ToolServer host.
    pub fn new_for_proxy(
        root_cwd: PathBuf,
        session_factory: Arc<dyn SessionContextFactory>,
        hub_config: HubConfig,
        auth_provider: echo_agent_computer_hub_sdk::SharedAuthProvider,
        server_metadata: Option<serde_json::Value>,
        status_config: crate::status_config::StatusConfig,
        tool_config: ToolServerConfig,
    ) -> Self {
        Self {
            root_cwd,
            default_tool_config: tool_config,
            respect_gitignore: false,
            memory_config: None,
            event_buffer_capacity: crate::config::DEFAULT_EVENT_BUFFER_CAPACITY,
            session_factory,
            hook_global_sources: vec![],
            hook_project_sources: vec![],
            skills_config: Default::default(),
            plugin_discovery_config: Default::default(),
            auth_provider: Some(auth_provider),
            hub_config: Some(hub_config),
            server_metadata,
            project_lsp_trusted: true,
            require_explicit_toolset: false,
            confine_fs_to_workspace_root: false,
            status_config,
        }
    }
}
/// Configuration for spawning a subagent session within a workspace.
#[derive(Clone)]
#[non_exhaustive]
pub struct AgentSessionConfig {
    /// Unique agent session id. Must be non-empty.
    pub agent_id: String,
    /// Filesystem isolation strategy.
    pub isolation: IsolationMode,
    /// Capability mode applied to this session's toolset.
    pub capability_mode: CapabilityMode,
    /// Per-fork tool override. `None` inherits from parent.
    pub tool_config: Option<ToolServerConfig>,
    /// Maximum recursion depth for subagent nesting.
    pub max_depth: u32,
    /// Working directory override. `None` inherits the parent's `cwd`.
    pub cwd_override: Option<PathBuf>,
    /// Extra env vars to layer on top of the parent's `session_env`.
    pub extra_env: HashMap<String, String>,
    /// Parent session to inherit from. Required.
    pub parent_session_id: Option<String>,
}
impl AgentSessionConfig {
    /// Construct a config with the supplied `agent_id` and otherwise
    /// minimal/permissive defaults.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            isolation: IsolationMode::None,
            capability_mode: CapabilityMode::ReadWrite,
            tool_config: None,
            max_depth: u32::MAX,
            cwd_override: None,
            extra_env: HashMap::new(),
            parent_session_id: None,
        }
    }
}
/// WARNING: `tool_config` is intentionally redacted from `Debug` output
/// because `ToolServerConfig.tools[*].params` may contain credentials.
impl std::fmt::Debug for AgentSessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSessionConfig")
            .field("agent_id", &self.agent_id)
            .field("isolation", &self.isolation)
            .field("capability_mode", &self.capability_mode)
            .field(
                "tool_config",
                if self.tool_config.is_some() {
                    &"Some(<redacted>)"
                } else {
                    &"None"
                },
            )
            .field("max_depth", &self.max_depth)
            .field("cwd_override", &self.cwd_override)
            .field("extra_env", &self.extra_env)
            .field("parent_session_id", &self.parent_session_id)
            .finish()
    }
}
/// A single hook source: either a JSON settings file or a directory of
/// `*.json` hook files. Maps 1:1 to [`echo_agent_hooks::discovery::HookSource`]
/// but uses owned `PathBuf` so the config struct is `'static`.
#[derive(Debug, Clone)]
pub enum HookSourceConfig {
    /// A single JSON settings file (e.g. `~/.claude/settings.json`).
    SettingsFile(PathBuf),
    /// A directory of `*.json` hook files (e.g. `~/.echo-agent/hooks/`).
    Directory(PathBuf),
}
/// Filesystem isolation strategy for a forked session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IsolationMode {
    /// No isolation: subagent shares the parent's working tree.
    #[default]
    None,
    /// Run the subagent in a copy-on-write git worktree.
    Worktree,
    /// Run the subagent inside a sandbox/container.
    Sandbox,
}
#[cfg(test)]
mod tests {
    use super::WorkspaceServerMetadata;
    #[test]
    fn workspace_server_metadata_serializes_all_present_fields() {
        let meta = WorkspaceServerMetadata {
            sandbox_id: Some("sb-123".to_owned()),
            session_id: Some("11111111-1111-1111-1111-111111111111".to_owned()),
            provider_id: Some("test-provider".to_owned()),
            launch_id: Some("33333333-3333-3333-3333-333333333333".to_owned()),
        };
        let value = serde_json::to_value(&meta).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "sandbox_id": "sb-123",
                "session_id": "11111111-1111-1111-1111-111111111111",
                "provider_id": "test-provider",
                "launch_id": "33333333-3333-3333-3333-333333333333",
            })
        );
    }
    #[test]
    fn workspace_server_metadata_omits_none_fields() {
        let meta = WorkspaceServerMetadata {
            sandbox_id: Some("sb-123".to_owned()),
            session_id: None,
            provider_id: None,
            launch_id: None,
        };
        let value = serde_json::to_value(&meta).unwrap();
        assert_eq!(value, serde_json::json!({ "sandbox_id": "sb-123" }));
        let empty = serde_json::to_value(WorkspaceServerMetadata::default()).unwrap();
        assert_eq!(empty, serde_json::json!({}));
    }
    #[test]
    fn workspace_server_metadata_deserializes_legacy_payload_without_new_fields() {
        let legacy = serde_json::json!({
            "sandbox_id": "sb-legacy",
            "cwd": "/workspace",
            "mode": "remote",
        });
        let meta: WorkspaceServerMetadata = serde_json::from_value(legacy).unwrap();
        assert_eq!(meta.sandbox_id.as_deref(), Some("sb-legacy"));
        assert_eq!(meta.session_id, None);
        assert_eq!(meta.provider_id, None);
    }
    #[test]
    fn workspace_server_metadata_round_trips_with_new_fields() {
        let meta = WorkspaceServerMetadata {
            sandbox_id: Some("sb-123".to_owned()),
            session_id: Some("22222222-2222-2222-2222-222222222222".to_owned()),
            provider_id: Some("test-provider".to_owned()),
            launch_id: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: WorkspaceServerMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sandbox_id, meta.sandbox_id);
        assert_eq!(back.session_id, meta.session_id);
        assert_eq!(back.provider_id, meta.provider_id);
    }
    #[test]
    fn workspace_server_metadata_deserializes_partial_new_fields() {
        let only_session = serde_json::json!({
            "sandbox_id": "sb-1",
            "session_id": "33333333-3333-3333-3333-333333333333",
        });
        let meta: WorkspaceServerMetadata = serde_json::from_value(only_session).unwrap();
        assert_eq!(
            meta.session_id.as_deref(),
            Some("33333333-3333-3333-3333-333333333333")
        );
        assert_eq!(meta.provider_id, None);
        let only_provider = serde_json::json!({
            "sandbox_id": "sb-1",
            "provider_id": "test-provider",
        });
        let meta: WorkspaceServerMetadata = serde_json::from_value(only_provider).unwrap();
        assert_eq!(meta.provider_id.as_deref(), Some("test-provider"));
        assert_eq!(meta.session_id, None);
    }
    #[test]
    fn workspace_server_metadata_reads_start_path_shaped_payload() {
        let start_path = serde_json::json!({
            "cwd": "/workspace",
            "mode": "remote",
            "sandbox_id": "sb-start",
            "session_id": "44444444-4444-4444-4444-444444444444",
            "provider_id": "test-provider",
        });
        let meta: WorkspaceServerMetadata = serde_json::from_value(start_path).unwrap();
        assert_eq!(meta.sandbox_id.as_deref(), Some("sb-start"));
        assert_eq!(
            meta.session_id.as_deref(),
            Some("44444444-4444-4444-4444-444444444444")
        );
        assert_eq!(meta.provider_id.as_deref(), Some("test-provider"));
    }
    #[test]
    fn merge_session_metadata_builds_struct_from_env_on_none_branch() {
        let merged =
            WorkspaceServerMetadata::merge_session_metadata(None, Some("sess-1".to_owned()))
                .unwrap();
        assert_eq!(merged, serde_json::json!({ "session_id": "sess-1" }));
        let empty = WorkspaceServerMetadata::merge_session_metadata(None, None).unwrap();
        assert_eq!(empty, serde_json::json!({}));
    }
    #[test]
    fn merge_session_metadata_overlays_into_object_without_clobbering() {
        let base = serde_json::json!({ "sandbox_id": "sb-9", "mode": "remote" });
        let merged =
            WorkspaceServerMetadata::merge_session_metadata(Some(base), Some("env-id".to_owned()))
                .unwrap();
        assert_eq!(
            merged,
            serde_json::json!({
                "sandbox_id": "sb-9",
                "mode": "remote",
                "session_id": "env-id",
            })
        );
        let explicit = serde_json::json!({ "session_id": "explicit" });
        let merged = WorkspaceServerMetadata::merge_session_metadata(
            Some(explicit),
            Some("env-id".to_owned()),
        )
        .unwrap();
        assert_eq!(merged, serde_json::json!({ "session_id": "explicit" }));
    }
    #[test]
    fn merge_session_metadata_leaves_object_untouched_when_no_env_id() {
        let base = serde_json::json!({ "sandbox_id": "sb-9" });
        let merged =
            WorkspaceServerMetadata::merge_session_metadata(Some(base.clone()), None).unwrap();
        assert_eq!(merged, base);
    }
    #[test]
    fn merge_session_metadata_non_object_is_returned_unchanged() {
        let scalar = serde_json::json!("just-a-string");
        let merged = WorkspaceServerMetadata::merge_session_metadata(
            Some(scalar.clone()),
            Some("env-id".to_owned()),
        )
        .unwrap();
        assert_eq!(merged, scalar);
    }
    #[test]
    fn merge_session_metadata_treats_empty_env_id_as_absent() {
        let none_branch =
            WorkspaceServerMetadata::merge_session_metadata(None, Some(String::new())).unwrap();
        assert_eq!(none_branch, serde_json::json!({}));
        let base = serde_json::json!({ "sandbox_id": "sb-9" });
        let overlay = WorkspaceServerMetadata::merge_session_metadata(
            Some(base.clone()),
            Some(String::new()),
        )
        .unwrap();
        assert_eq!(overlay, base);
    }
    #[test]
    fn workspace_server_metadata_rejects_wrong_typed_field() {
        let bad = serde_json::json!({ "sandbox_id": "sb-1", "session_id": 42 });
        let result: Result<WorkspaceServerMetadata, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }
}
