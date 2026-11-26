//! First-party tool normalization — the `ToolInput`-coupled projection on top
//! of the [`crate::tool_taxonomy`] leaf. Projects the agent's **typed**
//! [`ToolInput`] into the canonical input dict ([`canonical_input`]) and
//! resolves identity from registered metadata ([`tool_identity_of`]). The typed
//! value is the source of truth — robust to serde renames, exhaustiveness-checked.
use crate::registry::types::FinalizedToolset;
use crate::tool_taxonomy::field;
pub use crate::tool_taxonomy::{CanonicalToolMeta, ToolIdentity};
use crate::types::tool_io::ToolInput;
use crate::types::tool_metadata::ToolMetadata;
use serde::Serialize;
/// Resolve [`ToolIdentity`] from a tool's registered metadata.
pub fn tool_identity_of(metadata: &dyn ToolMetadata) -> ToolIdentity {
    let kind = metadata.kind();
    ToolIdentity {
        tool_kind: kind,
        namespace: metadata.tool_namespace(),
        presentation_name: kind.presentation_name(),
        read_only: metadata.is_read_only(),
    }
}
/// Resolve `wire_name` in `toolset` and merge the canonical `echo.agent/tool` object
/// into `existing` (see [`CanonicalToolMeta::merge_into`]). Returns `existing`
/// unchanged when the tool is unknown (uninitialized MCP, backend-hosted), so
/// markers like `bash_mode`/`backend` are never clobbered. This is the harness
/// entry point for stamping tool-call `_meta`.
pub fn merge_tool_meta(
    toolset: &FinalizedToolset,
    existing: Option<serde_json::Value>,
    wire_name: &str,
    parsed: Option<&ToolInput>,
) -> Option<serde_json::Value> {
    match toolset.tool_identity(wire_name) {
        Some(identity) => {
            let meta =
                CanonicalToolMeta::new(wire_name, &identity, parsed.and_then(canonical_input));
            Some(meta.merge_into(existing))
        }
        None => existing,
    }
}
/// Normalize a read offset to the 1-indexed canonical line. Readers
/// allow negative (from-end) offsets, which have no 1-indexed
/// equivalent and are dropped (consumers read `raw_input`); `0` coalesces to
/// `1`. Shared by [`canonical_input`] and the harness ACP location line so a
/// single tool-call event never exposes two different start lines.
pub fn norm_offset_i64(offset: Option<i64>) -> Option<u64> {
    match offset {
        Some(o) if o >= 0 => Some(o.max(1) as u64),
        _ => None,
    }
}
/// Project a tool's **typed** input into the harness-independent `input` dict of
/// the `echo.agent/tool` `_meta` object. Equivalent tools across toolsets emit the
/// same keys with the same meaning (a harness may add an extra key).
///
/// Returns `None` for tools with no stable cross-harness shape (MCP / dynamic /
/// codex / hashline / media / control-flow); the caller then omits `input`.
/// Absent optional fields are omitted (never `null`). Bulky payload fields
/// (edit `old_string`/`new_string`, full write contents) are never projected —
/// consumers read them from `raw_input`. Keys come from [`field`]; the match is
/// exhaustive so a new `ToolInput` variant must decide here.
pub fn canonical_input(input: &ToolInput) -> Option<serde_json::Value> {
    use serde_json::Value;
    /// Required field — omitted on serialization failure (absent fields are
    /// contract-covered: consumers fall back to `raw_input`).
    fn req(v: impl Serialize) -> Option<Value> {
        serde_json::to_value(v).ok()
    }
    /// Optional field — `None` is dropped, never serialized as `null`.
    fn opt<T: Serialize>(v: Option<T>) -> Option<Value> {
        v.and_then(|v| serde_json::to_value(v).ok())
    }
    fn obj(pairs: impl IntoIterator<Item = (&'static str, Option<Value>)>) -> Value {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            if let Some(v) = v {
                m.insert(k.to_string(), v);
            }
        }
        Value::Object(m)
    }
    Some(match input {
        ToolInput::ReadFile(r) => obj([
            (field::PATH, req(&r.path)),
            (field::OFFSET, opt(norm_offset_i64(r.offset))),
            (field::LIMIT, opt(r.limit)),
        ]),
        ToolInput::Bash(b) => obj([
            (field::COMMAND, req(&b.command)),
            (field::DESCRIPTION, req(&b.description)),
        ]),
        ToolInput::SearchReplace(s) => obj([(field::PATH, req(&s.file_path))]),
        ToolInput::Write(w) => obj([(field::PATH, req(&w.file_path))]),
        ToolInput::ListDir(l) => obj([(field::DIRECTORY, req(&l.target_directory))]),
        ToolInput::Grep(g) => obj([
            (field::PATTERN, req(&g.pattern)),
            (field::PATH, opt(g.path.as_ref())),
        ]),
        ToolInput::TodoWrite(_)
        | ToolInput::Skill(_)
        | ToolInput::MCPTool(_)
        | ToolInput::TaskOutput(_)
        | ToolInput::WaitTasks(_)
        | ToolInput::KillTask(_)
        | ToolInput::Task(_)
        | ToolInput::WebSearch(_)
        | ToolInput::ImageGen(_)
        | ToolInput::ImageEdit(_)
        | ToolInput::ImageToVideo(_)
        | ToolInput::ReferenceToVideo(_)
        | ToolInput::WebFetch(_)
        | ToolInput::ApplyPatch(_)
        | ToolInput::HashlineEdit(_)
        | ToolInput::CodexReadFile(_)
        | ToolInput::CodexListDir(_)
        | ToolInput::CodexGrepFiles(_)
        | ToolInput::MemorySearch(_)
        | ToolInput::MemoryGet(_)
        | ToolInput::SearchTool(_)
        | ToolInput::UseTool(_)
        | ToolInput::EnterPlanMode(_)
        | ToolInput::ExitPlanMode(_)
        | ToolInput::AskUserQuestion(_)
        | ToolInput::Lsp(_)
        | ToolInput::Monitor(_)
        | ToolInput::SchedulerCreate(_)
        | ToolInput::SchedulerDelete(_)
        | ToolInput::SchedulerList(_)
        | ToolInput::UpdateGoal(_)
        | ToolInput::Workflow(_)
        | ToolInput::Dynamic(_) => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parse(v: serde_json::Value) -> ToolInput {
        serde_json::from_value(v).expect("valid ToolInput")
    }
    #[test]
    fn canonical_omits_absent_options_not_null() {
        let echoagent = parse(serde_json::json!({"variant":"ReadFile","target_file":"/a"}));
        let g = canonical_input(&echoagent).unwrap();
        let keys: Vec<&String> = g.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec!["path"],
            "absent offset/limit must be omitted, not null"
        );
    }
}
