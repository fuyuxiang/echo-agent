//! Validation of [`ToolConfigEntry`](crate::ToolConfigEntry) fields,
//! shared so the backend's save-time check cannot drift from what the
//! tools server enforces at finalize/bind. Errors carry the offending input
//! so callers can render gRPC violations without re-parsing.

use echo_agent_tool_protocol::ToolId;
use serde_json::{Map, Value};

/// Why a [`ToolConfigEntry`](crate::ToolConfigEntry) is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolConfigEntryErrorKind {
    /// Not valid JSON. Includes an explicitly-set empty string: proto3
    /// `optional` tracks presence, so `Some("")` is rejected, not unset.
    ParamsJsonParse { error: String, raw: String },
    /// Valid JSON but not an object.
    ParamsJsonNotObject { value: Value },
    /// `name_override` is not a valid `ToolId` (charset/length contract).
    NameOverrideInvalid { name: String, error: String },
}

/// Validation error for one entry in a tool-config list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolConfigEntryError {
    pub index: usize,
    pub tool_id: String,
    pub kind: ToolConfigEntryErrorKind,
}

impl ToolConfigEntryError {
    /// Request field path of the failing field, e.g. `tools[3].params_json`.
    pub fn field_path(&self) -> String {
        match self.kind {
            ToolConfigEntryErrorKind::ParamsJsonParse { .. }
            | ToolConfigEntryErrorKind::ParamsJsonNotObject { .. } => {
                format!("tools[{}].params_json", self.index)
            }
            ToolConfigEntryErrorKind::NameOverrideInvalid { .. } => {
                format!("tools[{}].name_override", self.index)
            }
        }
    }
}

impl std::fmt::Display for ToolConfigEntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ToolConfigEntryErrorKind::ParamsJsonParse { error, .. } => write!(
                f,
                "{}: {} failed to parse JSON: {error}",
                self.tool_id,
                self.field_path()
            ),
            ToolConfigEntryErrorKind::ParamsJsonNotObject { .. } => write!(
                f,
                "{}: {} must be a JSON object",
                self.tool_id,
                self.field_path()
            ),
            ToolConfigEntryErrorKind::NameOverrideInvalid { name, error } => write!(
                f,
                "{}: {} is not a valid tool name ({name:?}): {error}",
                self.tool_id,
                self.field_path()
            ),
        }
    }
}

impl std::error::Error for ToolConfigEntryError {}

/// Parse and validate a `params_json`, returning the decoded object (or
/// `None` when unset). `index`/`tool_id` are only used for error reporting.
pub fn parse_params_json(
    index: usize,
    tool_id: &str,
    params_json: Option<&str>,
) -> Result<Option<Map<String, Value>>, ToolConfigEntryError> {
    let Some(raw) = params_json else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(raw).map_err(|err| ToolConfigEntryError {
        index,
        tool_id: tool_id.to_owned(),
        kind: ToolConfigEntryErrorKind::ParamsJsonParse {
            error: err.to_string(),
            raw: raw.to_owned(),
        },
    })?;
    match value {
        Value::Object(object) => Ok(Some(object)),
        other => Err(ToolConfigEntryError {
            index,
            tool_id: tool_id.to_owned(),
            kind: ToolConfigEntryErrorKind::ParamsJsonNotObject { value: other },
        }),
    }
}

/// Validates a `name_override` against the `ToolId` charset/length contract,
/// mirroring [`parse_params_json`] as the shared source of truth.
pub fn validate_name_override(
    index: usize,
    tool_id: &str,
    name_override: Option<&str>,
) -> Result<(), ToolConfigEntryError> {
    let Some(name) = name_override else {
        return Ok(());
    };
    ToolId::new(name).map_err(|err| ToolConfigEntryError {
        index,
        tool_id: tool_id.to_owned(),
        kind: ToolConfigEntryErrorKind::NameOverrideInvalid {
            name: name.to_owned(),
            error: err.to_string(),
        },
    })?;
    Ok(())
}

/// Returns the first entry whose `id` is not in `allowed_ids`, as
/// `(index, id)`, or `None` when all ids are allowed.
///
/// Pure so backend save-time validation and any future consumer share one rule.
pub fn first_unknown_tool_id<'a>(
    entries: &'a [crate::ToolConfigEntry],
    allowed_ids: &std::collections::HashSet<String>,
) -> Option<(usize, &'a str)> {
    entries
        .iter()
        .enumerate()
        .find(|(_, entry)| !allowed_ids.contains(&entry.id))
        .map(|(index, entry)| (index, entry.id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_params_is_ok_none() {
        assert_eq!(parse_params_json(0, "EchoAgentBuild:grep", None), Ok(None));
    }

    #[test]
    fn valid_object_is_returned() {
        let parsed =
            parse_params_json(0, "EchoAgentBuild:grep", Some(r#"{"max_results":50}"#)).unwrap();
        assert_eq!(
            parsed,
            Some(
                serde_json::json!({"max_results": 50})
                    .as_object()
                    .unwrap()
                    .clone()
            )
        );
    }

    #[test]
    fn empty_string_is_a_parse_error() {
        let err = parse_params_json(3, "EchoAgentBuild:grep", Some("")).unwrap_err();
        assert_eq!(err.index, 3);
        assert_eq!(err.field_path(), "tools[3].params_json");
        assert!(matches!(
            err.kind,
            ToolConfigEntryErrorKind::ParamsJsonParse { .. }
        ));
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        let err = parse_params_json(0, "t", Some("{not json")).unwrap_err();
        assert!(matches!(
            err.kind,
            ToolConfigEntryErrorKind::ParamsJsonParse { raw, .. } if raw == "{not json"
        ));
    }

    #[test]
    fn non_object_json_is_rejected() {
        let err = parse_params_json(1, "t", Some("[1,2,3]")).unwrap_err();
        assert!(matches!(
            err.kind,
            ToolConfigEntryErrorKind::ParamsJsonNotObject {
                value: Value::Array(_)
            }
        ));
    }

    #[test]
    fn name_override_unset_or_valid_is_ok() {
        assert_eq!(
            validate_name_override(0, "EchoAgentBuild:grep", None),
            Ok(())
        );
        for name in ["search", "EchoAgentBuild:grep", "a-b_C9"] {
            assert_eq!(
                validate_name_override(0, "EchoAgentBuild:grep", Some(name)),
                Ok(()),
                "name={name:?}"
            );
        }
    }

    #[test]
    fn name_override_outside_charset_is_rejected() {
        for name in ["has space", "", "a:b:c", "dot.name"] {
            let err = validate_name_override(2, "EchoAgentBuild:grep", Some(name)).unwrap_err();
            assert_eq!(err.index, 2, "name={name:?}");
            assert_eq!(err.field_path(), "tools[2].name_override");
            assert!(
                matches!(
                    &err.kind,
                    ToolConfigEntryErrorKind::NameOverrideInvalid { name: n, .. } if n == name
                ),
                "name={name:?} kind={:?}",
                err.kind
            );
        }
    }

    fn entry(id: &str) -> crate::ToolConfigEntry {
        crate::ToolConfigEntry {
            id: id.to_owned(),
            ..Default::default()
        }
    }

    fn allowed(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn all_ids_present_returns_none() {
        let entries = [
            entry("EchoAgentBuild:grep"),
            entry("EchoAgentBuild:read_file"),
        ];
        let allowed = allowed(&[
            "EchoAgentBuild:grep",
            "EchoAgentBuild:read_file",
            "EchoAgentBuild:bash",
        ]);
        assert_eq!(first_unknown_tool_id(&entries, &allowed), None);
    }

    #[test]
    fn empty_entries_returns_none() {
        assert_eq!(
            first_unknown_tool_id(&[], &allowed(&["EchoAgentBuild:grep"])),
            None
        );
    }

    #[test]
    fn first_unknown_id_is_returned_with_index() {
        let entries = [
            entry("EchoAgentBuild:grep"),
            entry("EchoAgentBuild:nonexistent"),
            entry("EchoAgentBuild:also_missing"),
        ];
        let allowed = allowed(&["EchoAgentBuild:grep"]);
        assert_eq!(
            first_unknown_tool_id(&entries, &allowed),
            Some((1, "EchoAgentBuild:nonexistent"))
        );
    }
}
