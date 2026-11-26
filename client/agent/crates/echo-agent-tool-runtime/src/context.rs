//! Context types and the typed-extension store they share.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use echo_agent_tool_protocol::ToolCallId;

/// Open typed-extension store keyed by `TypeId`.
#[derive(Clone, Default)]
pub struct TypedExtensions {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl TypedExtensions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
        self
    }

    pub fn insert_arc<T: Send + Sync + 'static>(&mut self, value: Arc<T>) -> &mut Self {
        self.map.insert(TypeId::of::<T>(), value);
        self
    }

    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|arc| Arc::downcast::<T>(arc).ok())
    }

    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<T>())
    }

    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<Arc<T>> {
        self.map
            .remove(&TypeId::of::<T>())
            .and_then(|arc| Arc::downcast::<T>(arc).ok())
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Copy entries from `defaults` that are not already present in `self`.
    pub fn merge_defaults(&mut self, defaults: &TypedExtensions) {
        for (key, value) in &defaults.map {
            self.map.entry(*key).or_insert_with(|| value.clone());
        }
    }
}

/// Per-call context.
#[derive(Clone)]
pub struct ToolCallContext {
    pub call_id: ToolCallId,
    pub extensions: TypedExtensions,
}

impl Default for ToolCallContext {
    fn default() -> Self {
        Self {
            call_id: ToolCallId::new_v7(),
            extensions: TypedExtensions::new(),
        }
    }
}

impl ToolCallContext {
    pub fn new(call_id: ToolCallId) -> Self {
        Self {
            call_id,
            extensions: TypedExtensions::new(),
        }
    }

    /// Delegate to `self.extensions.insert()`.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) -> &mut Self {
        self.extensions.insert(value);
        self
    }

    /// Delegate to `self.extensions.get()`.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.extensions.get::<T>()
    }
}

/// Per-turn context consumed by [`crate::Tool::should_list`].
#[derive(Clone, Default)]
pub struct ListToolsContext {
    pub extensions: TypedExtensions,
}

impl ListToolsContext {
    pub fn new() -> Self {
        Self::default()
    }
}

// Runtime-blessed per-concept extensions. One type per concept so
// dispatchers install exactly what they have and tools depend on
// exactly what they need.

/// Working directory for relative path resolution.
#[derive(Clone, Debug)]
pub struct Cwd(pub PathBuf);

/// Opaque behaviour version. Tools that branch on this MUST treat
/// unknown values as a hard error.
#[derive(Clone, Debug)]
pub struct BehaviorVersion(pub String);

/// Distributed-trace correlation context (e.g. W3C `traceparent`).
///
/// Receive-side carrier only: stamped from the inbound wire value for
/// tool impls to read, never serialized back out.
#[derive(Clone, Debug)]
pub struct TraceContext(pub String);

/// Session ID context — identifies which hub session this call belongs to.
/// Used by multi-session tool servers to dispatch to the correct
/// per-session state.
#[derive(Clone, Debug)]
pub struct SessionContext(pub String);

/// Cooperative-cancellation handle for the current tool call. Tools MAY
/// poll/await this for graceful shutdown; the dispatcher also hard-cancels
/// by dropping the call future when it fires.
#[derive(Clone, Debug)]
pub struct Cancellation(pub tokio_util::sync::CancellationToken);

/// Per-user feature-flag bag attached as a [`ToolCallContext`] extension.
/// Dispatcher resolves; tools read. Default = "off" for every field so an
/// absent extension never accidentally opts a feature in. Extend by
/// adding fields with safe defaults; new fields need `#[serde(default)]`
/// so older `session.bind` payloads stay deserializable.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceViewerContext {
    /// When `true`, `BashTool` emits `bash_output_chunk` Progress frames.
    #[serde(default)]
    pub stream_tool_progress: bool,
}

/// Wire shape of the Computer Hub `session.bind` metadata — one definition
/// shared by the emitter (serializes) and the workspace consumer
/// (deserializes), so the two can't drift on field names/types.
///
/// Excludes anything not meant for the workspace (cached tool definitions,
/// and terminal-provisioning inputs like image/fuse/isolation) so they can
/// never reach the wire. Every field tolerates a missing/malformed value
/// (drops to default) to keep valid siblings and mixed-version compatibility.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceBindMetadata {
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub preset: Option<String>,
    /// Raw string; the workspace maps it to its own capability enum.
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub capability_mode: Option<String>,
    /// Explicit toolset in the echo-agent-tools gRPC wire shape. Empty = unset.
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tools: Vec<echo_agent_tools_api::ToolConfigEntry>,
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub viewer_ctx: Option<WorkspaceViewerContext>,
    /// Initial auto-approve (YOLO) state for the bound session. Omitted when
    /// unset (legacy emitters / wire compat with older workspace servers);
    /// consumers fail closed on `None`.
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub yolo_mode: Option<bool>,
    /// Optional/additive: omitted by emitters that don't yet write it.
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub manifest_version: Option<String>,
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub manifest_hash: Option<String>,
    /// Opt-in: forward SystemNotifications produced in this session to the gateway.
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_notifications: Option<bool>,
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub rpc_only: bool,
    /// Real guest session root (`/workspace/<conversation_id>`). When set,
    /// the workspace virtualizes that tree as `/workspace`.
    #[serde(
        default,
        deserialize_with = "ok_or_default",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_root: Option<String>,
}

/// Deserialize a field, falling back to its default on a malformed value
/// instead of failing the whole struct.
fn ok_or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

#[cfg(test)]
mod bind_metadata_tests {
    use super::WorkspaceBindMetadata;

    #[test]
    fn serialize_omits_empty_fields() {
        let md = WorkspaceBindMetadata::default();
        assert_eq!(serde_json::to_value(&md).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn round_trips_populated() {
        let md = WorkspaceBindMetadata {
            preset: Some("explore".to_owned()),
            capability_mode: Some("read_only".to_owned()),
            tools: vec![echo_agent_tools_api::ToolConfigEntry {
                id: "EchoAgentBuild:grep".to_owned(),
                ..Default::default()
            }],
            viewer_ctx: Some(super::WorkspaceViewerContext {
                stream_tool_progress: true,
            }),
            yolo_mode: Some(true),
            manifest_version: Some("v1".to_owned()),
            manifest_hash: Some("abc123".to_owned()),
            system_notifications: Some(true),
            rpc_only: true,
            session_root: Some("/workspace/conv-abc".to_owned()),
        };
        let value = serde_json::to_value(&md).unwrap();
        let back: WorkspaceBindMetadata = serde_json::from_value(value).unwrap();
        assert_eq!(back.preset.as_deref(), Some("explore"));
        assert_eq!(back.capability_mode.as_deref(), Some("read_only"));
        assert_eq!(back.tools.len(), 1);
        assert!(back.viewer_ctx.unwrap().stream_tool_progress);
        assert_eq!(back.yolo_mode, Some(true));
        assert_eq!(back.manifest_version.as_deref(), Some("v1"));
        assert_eq!(back.manifest_hash.as_deref(), Some("abc123"));
        assert_eq!(back.system_notifications, Some(true));
        assert!(back.rpc_only);
        assert_eq!(back.session_root.as_deref(), Some("/workspace/conv-abc"));
    }

    #[test]
    fn rpc_only_omitted_when_false_wire_compatible() {
        let md = WorkspaceBindMetadata::default();
        let value = serde_json::to_value(&md).unwrap();
        assert!(value.get("rpc_only").is_none());

        let md: WorkspaceBindMetadata =
            serde_json::from_value(serde_json::json!({"preset": "explore"})).unwrap();
        assert!(!md.rpc_only);

        let md: WorkspaceBindMetadata =
            serde_json::from_value(serde_json::json!({"rpc_only": true})).unwrap();
        assert!(md.rpc_only);
    }

    #[test]
    fn system_notifications_is_wire_compatible() {
        let md = WorkspaceBindMetadata::default();
        let value = serde_json::to_value(&md).unwrap();
        assert!(value.get("system_notifications").is_none());

        let md = WorkspaceBindMetadata {
            system_notifications: Some(true),
            ..Default::default()
        };
        let value = serde_json::to_value(&md).unwrap();
        let back: WorkspaceBindMetadata = serde_json::from_value(value).unwrap();
        assert_eq!(back.system_notifications, Some(true));

        let md: WorkspaceBindMetadata =
            serde_json::from_value(serde_json::json!({"preset": "explore"})).unwrap();
        assert!(md.system_notifications.is_none());
    }

    #[test]
    fn malformed_field_falls_back_to_default_keeping_siblings() {
        // `tools` is the wrong type and `capability_mode` is fine: the bad
        // field drops to default, the good sibling survives.
        let value = serde_json::json!({
            "preset": "explore",
            "capability_mode": "read_only",
            "tools": "not-a-list",
        });
        let md: WorkspaceBindMetadata = serde_json::from_value(value).unwrap();
        assert_eq!(md.preset.as_deref(), Some("explore"));
        assert_eq!(md.capability_mode.as_deref(), Some("read_only"));
        assert!(md.tools.is_empty());
    }

    #[test]
    fn legacy_payload_without_viewer_ctx_parses() {
        let md: WorkspaceBindMetadata =
            serde_json::from_value(serde_json::json!({"preset": "explore"})).unwrap();
        assert!(md.viewer_ctx.is_none());
    }

    #[test]
    fn session_root_is_wire_compatible() {
        let md = WorkspaceBindMetadata::default();
        let value = serde_json::to_value(&md).unwrap();
        assert!(value.get("session_root").is_none());

        let md: WorkspaceBindMetadata =
            serde_json::from_value(serde_json::json!({"session_root": "/workspace/c1"})).unwrap();
        assert_eq!(md.session_root.as_deref(), Some("/workspace/c1"));
    }
}
