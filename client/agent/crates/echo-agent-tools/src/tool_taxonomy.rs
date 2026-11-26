//! Tool taxonomy — the harness-independent vocabulary, identity, and canonical
//! `_meta` envelope.
//!
//! Depends only on `ToolKind`/`ToolNamespace` + `serde`/`serde_json` (no
//! `ToolInput`, proto, or runtime). A future `echo-agent-tool-taxonomy` leaf crate
//! would need those two (dependency-free) enums moved here too — coherence ties
//! the inherent impls to the enum definitions. The `ToolInput`-coupled
//! projection lives in [`crate::normalization`].
use crate::types::tool::{ToolKind, ToolNamespace};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
/// Canonical input field names — the one vocabulary every harness normalizes
/// onto. Emit canonical keys through these so the wire contract has one source.
pub mod field {
    pub const PATH: &str = "path";
    pub const OFFSET: &str = "offset";
    pub const LIMIT: &str = "limit";
    pub const COMMAND: &str = "command";
    pub const DESCRIPTION: &str = "description";
    pub const CWD: &str = "cwd";
    pub const DIRECTORY: &str = "directory";
    pub const PATTERN: &str = "pattern";
}
/// The single `_meta` key holding the canonical tool identity as one nested
/// object (mirroring `echo.agent/mcp_tool`). Consumers deserialize it into
/// [`CanonicalToolMeta`].
pub const TOOL_META_KEY: &str = "echo.agent/tool";
/// Version of the canonical tool `_meta` contract. Bump on any breaking change
/// to keys or value shapes so consumers can adapt.
pub const TOOL_META_VERSION: u32 = 1;
impl ToolKind {
    /// Unified, harness-independent display label for this semantic kind. A pure
    /// function of the kind, so equivalent tools across toolsets share it
    /// (`read_file` and `Read` → `Read`; `run_terminal_cmd` and `Shell` →
    /// `Run Command`). Display only; the model's tool name is `name` in
    /// `echo.agent/tool`. Exhaustive, so a new `ToolKind` must add a label to compile.
    pub fn presentation_name(self) -> &'static str {
        match self {
            ToolKind::Read => "Read",
            ToolKind::Edit => "Edit",
            ToolKind::Delete => "Delete",
            ToolKind::Write => "Write",
            ToolKind::Move => "Move",
            ToolKind::ListDir => "List Files",
            ToolKind::List => "List Files",
            ToolKind::Search => "Search",
            ToolKind::Lsp => "Code Intelligence",
            ToolKind::Execute => "Run Command",
            ToolKind::Plan => "Plan",
            ToolKind::WebSearch => "Web Search",
            ToolKind::WebFetch => "Web Fetch",
            ToolKind::BackgroundTaskAction => "Background Task",
            ToolKind::WaitTasksAction => "Wait for Tasks",
            ToolKind::KillTaskAction => "Kill Task",
            ToolKind::Skill => "Skill",
            ToolKind::MemorySearch => "Memory Search",
            ToolKind::MemoryGet => "Memory Read",
            ToolKind::Task => "Subagent",
            ToolKind::EnterPlan => "Enter Plan Mode",
            ToolKind::ExitPlan => "Exit Plan Mode",
            ToolKind::AskUser => "Ask User",
            ToolKind::ImageGen => "Generate Image",
            ToolKind::VideoGen => "Generate Video",
            ToolKind::ImageToVideo => "Generate Video",
            ToolKind::ReferenceToVideo => "Generate Video",
            ToolKind::DeployApp => "Deploy App",
            ToolKind::InitOrUpdateApp => "Init or Update App",
            ToolKind::SearchTool => "Search Tools",
            ToolKind::UseTool => "Use Tool",
            ToolKind::Monitor => "Monitor",
            ToolKind::GoalUpdate => "Update Goal",
            ToolKind::Workflow => "Workflow",
            ToolKind::Other => "Tool",
        }
    }
    /// Whether this kind only reads (no workspace or external mutation) by
    /// default. The kind-level default for `ToolMetadata::is_read_only`, which
    /// individual tools may override. Exhaustive (no `_`) so a new kind must
    /// classify itself rather than silently defaulting to "mutating".
    pub fn is_read_only(self) -> bool {
        match self {
            ToolKind::Read
            | ToolKind::Search
            | ToolKind::Lsp
            | ToolKind::ListDir
            | ToolKind::List
            | ToolKind::MemorySearch
            | ToolKind::MemoryGet
            | ToolKind::WebSearch
            | ToolKind::WebFetch
            | ToolKind::EnterPlan
            | ToolKind::ExitPlan
            | ToolKind::AskUser => true,
            ToolKind::Edit
            | ToolKind::Delete
            | ToolKind::Write
            | ToolKind::Move
            | ToolKind::Execute
            | ToolKind::Plan
            | ToolKind::BackgroundTaskAction
            | ToolKind::WaitTasksAction
            | ToolKind::KillTaskAction
            | ToolKind::Skill
            | ToolKind::Task
            | ToolKind::ImageGen
            | ToolKind::VideoGen
            | ToolKind::ImageToVideo
            | ToolKind::ReferenceToVideo
            | ToolKind::DeployApp
            | ToolKind::InitOrUpdateApp
            | ToolKind::SearchTool
            | ToolKind::UseTool
            | ToolKind::Monitor
            | ToolKind::GoalUpdate
            | ToolKind::Workflow
            | ToolKind::Other => false,
        }
    }
}
/// First-party tool wire names whose argument streams are long enough for a
/// writing-phase spinner label to be visible (file bodies, edit strings,
/// shell scripts, prompts), paired with their [`ToolKind`].
///
/// Public so clients can pin that every entry gets non-fallback display copy
/// — a spelling added here without client copy would otherwise silently keep
/// the raw-name fallback.
pub const WRITING_TOOL_WIRE_NAMES: &[(&str, ToolKind)] = &[
    ("write", ToolKind::Write),
    ("search_replace", ToolKind::Edit),
    ("edit", ToolKind::Edit),
    ("hashline_edit", ToolKind::Edit),
    ("apply_patch", ToolKind::Edit),
    ("run_terminal_command", ToolKind::Execute),
    ("run_terminal_cmd", ToolKind::Execute),
    ("bash", ToolKind::Execute),
    ("todo_write", ToolKind::Plan),
    ("todowrite", ToolKind::Plan),
    ("workflow", ToolKind::Workflow),
    ("image_gen", ToolKind::ImageGen),
    ("image_edit", ToolKind::ImageGen),
    ("image_to_video", ToolKind::ImageToVideo),
    ("reference_to_video", ToolKind::ReferenceToVideo),
    ("ask_user_question", ToolKind::AskUser),
];
/// [`ToolKind`] of a wire name in [`WRITING_TOOL_WIRE_NAMES`].
///
/// Keyed by wire name because that is all a client has while
/// `tool_call_delta_chunk`s stream. Best-effort by design: wire names are
/// client-renameable, so unknown names return `None` and callers fall back to
/// showing the raw name. Not a general name→kind resolver — read-style tools
/// with tiny argument payloads are deliberately absent, as are the MCP
/// dispatch tools (`use_tool`/`search_tool`), which clients special-case by
/// name constant.
pub fn writing_tool_kind(wire_name: &str) -> Option<ToolKind> {
    WRITING_TOOL_WIRE_NAMES
        .iter()
        .find(|(name, _)| *name == wire_name)
        .map(|&(_, kind)| kind)
}
impl schemars::JsonSchema for ToolKind {
    fn schema_name() -> Cow<'static, str> {
        "ToolKind".into()
    }
    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        use strum::IntoEnumIterator;
        let known = Self::iter()
            .filter_map(|k| serde_json::to_value(k).ok())
            .filter_map(|v| v.as_str().map(|s| format!("`{s}`")))
            .collect::<Vec<_>>()
            .join(", ");
        schemars::json_schema!({
            "type": "string",
            "description": format!(
                "Categorizes what a tool does at a high level. Open set — consumers must \
                 tolerate unknown values (Rust deserializes them to `other` via \
                 `#[serde(other)]`). Known values: {known}."
            ),
        })
    }
}
/// Canonical identity for a tool call, resolved from a tool's registered
/// metadata by its client-facing wire name.
///
/// Harness-independent. `tool_kind` is the authoritative `metadata.kind()`.
#[derive(Debug, Clone, Copy)]
pub struct ToolIdentity {
    pub tool_kind: ToolKind,
    pub namespace: ToolNamespace,
    pub presentation_name: &'static str,
    pub read_only: bool,
}
/// The canonical tool-identity envelope, attached to a tool-call event `_meta`
/// as one nested object under [`TOOL_META_KEY`].
///
/// ```json
/// "echo.agent/tool": {
///   "version": 1,
///   "name": "read_file",
///   "kind": "read",
///   "namespace": "echo_agent_build",
///   "label": "Read",
///   "read_only": true,
///   "input": { "path": "..." }
/// }
/// ```
///
/// Consumer contract:
/// - **`label`** is the cross-harness grouping/display key: equivalent tools
///   share it (echoagent `read_file` → `"Read"`).
/// - **`kind`** is a finer discriminator (`metadata.kind()`), *not* guaranteed
///   equal for equivalent ops across harnesses (listing is `list` in one
///   toolset, `list_dir` in another); prefer `label` to join, tolerate unknowns.
/// - **`name`** is the harness-specific model-facing name; for diagnostics.
///   For harness-initiated events (e.g. the `bash_mode` marker), `raw_input`
///   is not guaranteed to match `name`'s schema.
/// - **`input`** is a canonical *projection*, not a mirror: cross-harness keys
///   only, so some raw fields are intentionally dropped (e.g. grep flags,
///   `replace_all`), and bulky payload
///   fields (edit `old_string`/`new_string`, full write contents) are never
///   projected — read them from `raw_input`. It is omitted entirely
///   when no stable shape exists (MCP / dynamic / out-of-scope). When a field or
///   the whole dict is absent, fall back to `raw_input` on this or an earlier
///   update for the same `tool_call_id` (some updates, e.g. a parse failure,
///   carry neither and rely on the merge below).
/// - **Lifecycle:** updates for one call share a `tool_call_id` — merge across
///   them (last write wins); `input` may arrive on a later update.
/// - **Versioning:** additive changes (new object fields, new `kind` / `label`
///   values) don't bump `version`. Unknown `kind` degrades to `"other"`;
///   `namespace` is a closed enum (no `other` sink), so a new toolset fails
///   strict typed deserialization of the whole envelope — intentional, to force
///   typed consumers with exhaustive matches to update. Out-of-tree consumers
///   should read `namespace` loosely (as a string) and, on any `echo.agent/tool`
///   parse failure, treat it as absent and fall back to `raw_input` + the ACP
///   `kind`. `version` bumps only on removal or meaning change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CanonicalToolMeta {
    pub version: u32,
    pub name: String,
    pub kind: ToolKind,
    pub namespace: ToolNamespace,
    pub label: Cow<'static, str>,
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
}
impl CanonicalToolMeta {
    /// Build from a resolved identity + already-projected `input`. `ToolInput`-free
    /// so the type stays a leaf (projection lives in `normalization`).
    pub fn new(
        name: impl Into<String>,
        identity: &ToolIdentity,
        input: Option<serde_json::Value>,
    ) -> Self {
        Self {
            version: TOOL_META_VERSION,
            name: name.into(),
            kind: identity.tool_kind,
            namespace: identity.namespace,
            label: Cow::Borrowed(identity.presentation_name),
            read_only: identity.read_only,
            input,
        }
    }
    /// Attach under [`TOOL_META_KEY`], preserving existing `_meta` keys
    /// (`bash_mode`, `backend`, `echo.agent/mcp_tool`, …).
    pub fn merge_into(&self, existing: Option<serde_json::Value>) -> serde_json::Value {
        debug_assert!(
            matches!(existing, None | Some(serde_json::Value::Object(_))),
            "_meta is always absent or an object"
        );
        let mut map = match existing {
            Some(serde_json::Value::Object(m)) => m,
            Some(other) => return other,
            None => serde_json::Map::new(),
        };
        let value = serde_json::to_value(self).expect("CanonicalToolMeta serializes");
        map.insert(TOOL_META_KEY.to_string(), value);
        serde_json::Value::Object(map)
    }
}
/// The published JSON Schema (draft-07) for the [`CanonicalToolMeta`] wire
/// envelope (`schema/tool_meta.schema.json`). Non-Rust consumers codegen from
/// it; kept in sync with the type by `tool_meta_schema_is_up_to_date`.
pub fn tool_meta_json_schema_str() -> &'static str {
    include_str!("../schema/tool_meta.schema.json")
}
#[cfg(test)]
mod tests {
    use super::*;
    fn identity(kind: ToolKind) -> ToolIdentity {
        ToolIdentity {
            tool_kind: kind,
            namespace: ToolNamespace::EchoAgentBuild,
            presentation_name: kind.presentation_name(),
            read_only: kind.is_read_only(),
        }
    }
    /// Every writing-visible spelling stays glued to its definition site:
    /// the map must agree with the live tool's `id()` and metadata `kind()`.
    #[test]
    fn writing_tool_kind_matches_definition_sites() {
        use crate::types::tool_metadata::ToolMetadata;
        use echo_agent_tool_runtime::Tool;
        fn covered<T: Tool + ToolMetadata>(tool: T) {
            assert_eq!(
                writing_tool_kind(tool.id().as_str()),
                Some(ToolMetadata::kind(&tool)),
                "writing_tool_kind drifted for `{}`",
                tool.id()
            );
        }
        covered(crate::implementations::echo_agent_build::SearchReplaceTool);
        covered(crate::implementations::echo_agent_build::BashTool);
        covered(crate::implementations::echo_agent_build::TodoWriteTool);
        covered(crate::implementations::echo_agent_build::WorkflowTool);
        covered(crate::implementations::echo_agent_build::ImageGenTool);
        covered(crate::implementations::echo_agent_build::ImageEditTool);
        covered(crate::implementations::echo_agent_build::ImageToVideoTool);
        covered(crate::implementations::echo_agent_build::ReferenceToVideoTool);
        covered(crate::implementations::echo_agent_build::AskUserQuestionTool);
        covered(crate::implementations::opencode::OpenCodeWriteTool);
        covered(crate::implementations::opencode::OpenCodeEditTool);
        covered(crate::implementations::opencode::OpenCodeBashTool);
        covered(crate::implementations::opencode::OpenCodeTodoWriteTool);
        covered(crate::implementations::codex::ApplyPatchTool);
        covered(crate::implementations::echo_agent_build_hashline::HashlineEditTool);
    }
    /// Spellings with no instantiable definition site in this crate
    /// (client-facing renames) and the deliberate absences.
    #[test]
    fn writing_tool_kind_renames_and_absences() {
        assert_eq!(
            writing_tool_kind("run_terminal_command"),
            Some(ToolKind::Execute)
        );
        assert_eq!(writing_tool_kind("read_file"), None);
        assert_eq!(writing_tool_kind("grep"), None);
        assert_eq!(writing_tool_kind("list_dir"), None);
        assert_eq!(writing_tool_kind(crate::USE_TOOL_NAME), None);
        assert_eq!(writing_tool_kind(crate::SEARCH_TOOL_NAME), None);
    }
    #[test]
    fn is_read_only_classifies_kinds() {
        assert!(ToolKind::Read.is_read_only());
        assert!(ToolKind::Search.is_read_only());
        assert!(ToolKind::List.is_read_only());
        assert!(!ToolKind::Edit.is_read_only());
        assert!(!ToolKind::Execute.is_read_only());
        assert!(!ToolKind::Delete.is_read_only());
    }
    #[test]
    fn namespace_round_trips_snake_case_with_pascal_aliases() {
        use strum::IntoEnumIterator;
        fn wire_and_pascal(ns: ToolNamespace) -> (&'static str, &'static str) {
            match ns {
                ToolNamespace::EchoAgentBuild => ("echo_agent_build", "EchoAgentBuild"),
                ToolNamespace::EchoAgentBuildConcise => {
                    ("echo_agent_build_concise", "EchoAgentBuildConcise")
                }
                ToolNamespace::EchoAgentBuildHashline => {
                    ("echo_agent_build_hashline", "EchoAgentBuildHashline")
                }
                ToolNamespace::Codex => ("codex", "Codex"),
                ToolNamespace::OpenCode => ("opencode", "OpenCode"),
                ToolNamespace::MCP => ("mcp", "MCP"),
            }
        }
        for ns in ToolNamespace::iter() {
            let (snake, pascal) = wire_and_pascal(ns);
            assert_eq!(serde_json::to_value(ns).unwrap(), serde_json::json!(snake));
            assert_eq!(
                serde_json::from_value::<ToolNamespace>(serde_json::json!(snake)).unwrap(),
                ns
            );
            assert_eq!(
                serde_json::from_value::<ToolNamespace>(serde_json::json!(pascal)).unwrap(),
                ns
            );
        }
    }
    #[test]
    fn unknown_kind_degrades_to_other() {
        let k: ToolKind = serde_json::from_value(serde_json::json!("teleport")).unwrap();
        assert_eq!(k, ToolKind::Other);
    }
    /// The published `kind` schema must stay an open string (codegen'd
    /// consumers would otherwise hard-fail on new kinds, contradicting the
    /// `#[serde(other)]` contract above). `namespace` stays intentionally
    /// closed — see the versioning contract on [`CanonicalToolMeta`].
    #[test]
    fn kind_schema_is_open_string_namespace_stays_closed() {
        let kind = serde_json::to_value(schemars::schema_for!(ToolKind)).unwrap();
        assert_eq!(kind["type"], "string");
        assert!(kind.get("enum").is_none(), "kind must not be a closed enum");
        assert!(
            kind["description"].as_str().unwrap().contains("`read`"),
            "known values must be listed in the description"
        );
        let ns = serde_json::to_value(schemars::schema_for!(ToolNamespace)).unwrap();
        assert!(ns.get("enum").is_some(), "namespace is a closed enum");
    }
    #[test]
    fn canonical_meta_wire_shape_round_trips() {
        let meta = CanonicalToolMeta::new(
            "read_file",
            &identity(ToolKind::Read),
            Some(serde_json::json!({ "path": "/a" })),
        );
        let t = serde_json::to_value(&meta).unwrap();
        assert_eq!(t["version"], serde_json::json!(TOOL_META_VERSION));
        assert_eq!(t["name"], "read_file");
        assert_eq!(t["kind"], "read");
        assert_eq!(t["namespace"], "echo_agent_build");
        assert_eq!(t["label"], "Read");
        assert_eq!(t["read_only"], true);
        assert_eq!(t["input"]["path"], "/a");
        assert_eq!(
            serde_json::from_value::<CanonicalToolMeta>(t).unwrap(),
            meta
        );
    }
    /// The checked-in schema (the artifact non-Rust consumers codegen from) must
    /// track the type. Regenerate with `UPDATE_TOOL_META_SCHEMA=1`.
    #[test]
    fn tool_meta_schema_is_up_to_date() {
        let generator = schemars::generate::SchemaSettings::draft07().into_generator();
        let schema = serde_json::to_value(generator.into_root_schema_for::<CanonicalToolMeta>())
            .expect("schema serializes");
        let generated = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
        if std::env::var("UPDATE_TOOL_META_SCHEMA").is_ok() {
            std::fs::write(
                concat!(env!("CARGO_MANIFEST_DIR"), "/schema/tool_meta.schema.json"),
                &generated,
            )
            .unwrap();
            return;
        }
        let mut expected: serde_json::Value =
            serde_json::from_str(tool_meta_json_schema_str()).expect("checked-in schema parses");
        if let Some(values) = expected["definitions"]["ToolNamespace"]["enum"].as_array_mut() {
            use std::collections::HashSet;
            use strum::IntoEnumIterator;
            let compiled: HashSet<String> = ToolNamespace::iter()
                .filter_map(|ns| {
                    serde_json::to_value(ns)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                })
                .collect();
            values.retain(|v| matches!(v.as_str(), Some(s) if compiled.contains(s)));
        }
        let expected = format!("{}\n", serde_json::to_string_pretty(&expected).unwrap());
        assert_eq!(
            generated, expected,
            "tool_meta.schema.json is stale; regenerate with UPDATE_TOOL_META_SCHEMA=1"
        );
    }
    #[test]
    fn merge_into_nests_under_one_key_and_preserves_existing() {
        let meta = CanonicalToolMeta::new("run_terminal_cmd", &identity(ToolKind::Execute), None);
        let merged = meta.merge_into(Some(serde_json::json!({"bash_mode": true})));
        let o = merged.as_object().unwrap();
        assert_eq!(o["bash_mode"], true, "existing meta must be preserved");
        let t = &o[TOOL_META_KEY];
        assert_eq!(t["kind"], "execute");
        assert!(t.get("input").is_none(), "absent input omitted");
    }
}
