//! Conversion from the gRPC wire config types (`echo-agent-tools-api`) to the
//! runtime registry types ([`ToolConfig`] / [`ToolServerConfig`]).
//!
//! The `params_json` parse/validation contract lives in
//! [`echo_agent_tools_api::config_validation`] so every consumer (this
//! converter, save-time config validation, ...) shares one source of
//! truth. The error types are re-exported here for back-compat.

use super::types::{ToolConfig, ToolServerConfig};

pub use echo_agent_tools_api::config_validation::{ToolConfigEntryError, ToolConfigEntryErrorKind};

/// Convert one wire [`echo_agent_tools_api::ToolConfigEntry`] to a runtime
/// [`ToolConfig`].
///
/// `index` is only used for error reporting. The result always has
/// `kind: None`: the wire format carries no capability kind, and
/// capability-mode filtering intentionally keeps baseline `kind: None` tools.
pub fn tool_config_from_entry(
    index: usize,
    entry: echo_agent_tools_api::ToolConfigEntry,
) -> Result<ToolConfig, ToolConfigEntryError> {
    let echo_agent_tools_api::ToolConfigEntry {
        id,
        params_json,
        name_override,
        params_name_overrides,
        behavior_version,
        description_override,
    } = entry;
    let params = echo_agent_tools_api::config_validation::parse_params_json(
        index,
        &id,
        params_json.as_deref(),
    )?;
    echo_agent_tools_api::config_validation::validate_name_override(
        index,
        &id,
        name_override.as_deref(),
    )?;
    Ok(ToolConfig {
        id,
        params,
        name_override,
        params_name_overrides: if params_name_overrides.is_empty() {
            None
        } else {
            Some(params_name_overrides)
        },
        description_override,
        behavior_version,
        kind: None,
    })
}

/// Convert a wire tool-config list to a runtime [`ToolServerConfig`].
/// Fails on the first invalid entry.
///
/// `behavior_preset` is always `None` (the `"current"` default); per-tool
/// `behavior_version` overrides on individual entries still apply.
pub fn tool_server_config_from_entries(
    entries: Vec<echo_agent_tools_api::ToolConfigEntry>,
) -> Result<ToolServerConfig, ToolConfigEntryError> {
    let tools = entries
        .into_iter()
        .enumerate()
        .map(|(idx, entry)| tool_config_from_entry(idx, entry))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ToolServerConfig {
        tools,
        behavior_preset: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> echo_agent_tools_api::ToolConfigEntry {
        echo_agent_tools_api::ToolConfigEntry {
            id: id.to_owned(),
            params_json: None,
            name_override: None,
            params_name_overrides: Default::default(),
            behavior_version: None,
            description_override: None,
        }
    }

    #[test]
    fn minimal_entry_converts_with_defaults() {
        let cfg = tool_config_from_entry(0, entry("EchoAgentBuild:read_file")).unwrap();
        assert_eq!(cfg.id, "EchoAgentBuild:read_file");
        assert_eq!(cfg.params, None);
        assert_eq!(cfg.name_override, None);
        assert_eq!(cfg.params_name_overrides, None);
        assert_eq!(cfg.description_override, None);
        assert_eq!(cfg.behavior_version, None);
        assert_eq!(cfg.kind, None, "wire entries never carry a kind");
    }

    #[test]
    fn fully_populated_entry_converts_field_by_field() {
        let mut e = entry("EchoAgentBuild:grep");
        e.params_json = Some(r#"{"max_results": 50}"#.to_owned());
        e.name_override = Some("search".to_owned());
        e.params_name_overrides =
            std::collections::HashMap::from([("pattern".to_owned(), "query".to_owned())]);
        e.behavior_version = Some("legacy-0.4.10".to_owned());
        e.description_override = Some("Search the codebase".to_owned());

        let cfg = tool_config_from_entry(0, e).unwrap();
        assert_eq!(
            cfg.params,
            Some(
                serde_json::json!({"max_results": 50})
                    .as_object()
                    .cloned()
                    .unwrap()
            )
        );
        assert_eq!(cfg.name_override.as_deref(), Some("search"));
        assert_eq!(
            cfg.params_name_overrides.as_ref().unwrap()["pattern"],
            "query"
        );
        assert_eq!(cfg.behavior_version.as_deref(), Some("legacy-0.4.10"));
        assert_eq!(
            cfg.description_override.as_deref(),
            Some("Search the codebase")
        );
    }

    #[test]
    fn invalid_params_json_is_a_parse_error() {
        let mut e = entry("EchoAgentBuild:bash");
        e.params_json = Some("{not json".to_owned());
        let err = tool_config_from_entry(3, e).unwrap_err();
        assert_eq!(err.index, 3);
        assert_eq!(err.tool_id, "EchoAgentBuild:bash");
        assert_eq!(err.field_path(), "tools[3].params_json");
        assert!(matches!(
            &err.kind,
            ToolConfigEntryErrorKind::ParamsJsonParse { raw, .. } if raw == "{not json"
        ));
    }

    #[test]
    fn non_object_params_json_is_a_type_error() {
        let mut e = entry("EchoAgentBuild:bash");
        e.params_json = Some("[1, 2]".to_owned());
        let err = tool_config_from_entry(1, e).unwrap_err();
        assert_eq!(
            err.kind,
            ToolConfigEntryErrorKind::ParamsJsonNotObject {
                value: serde_json::json!([1, 2])
            }
        );
        assert_eq!(err.field_path(), "tools[1].params_json");
    }

    #[test]
    fn name_override_valid_tool_id_charset_is_accepted() {
        for name in ["search", "EchoAgentBuild:grep", "a-b_C9"] {
            let mut e = entry("EchoAgentBuild:grep");
            e.name_override = Some(name.to_owned());
            let cfg = tool_config_from_entry(0, e).unwrap();
            assert_eq!(cfg.name_override.as_deref(), Some(name));
        }
    }

    #[test]
    fn name_override_outside_tool_id_charset_is_rejected() {
        for name in ["has space", "", "a:b:c", "emoji✨", "dot.name"] {
            let mut e = entry("EchoAgentBuild:grep");
            e.name_override = Some(name.to_owned());
            let err = tool_config_from_entry(2, e).unwrap_err();
            assert_eq!(err.index, 2, "name={name:?}");
            assert_eq!(err.tool_id, "EchoAgentBuild:grep");
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

    #[test]
    fn server_config_conversion_rejects_invalid_name_override_entry() {
        let mut bad = entry("EchoAgentBuild:grep");
        bad.name_override = Some("bad name".to_owned());
        let err = tool_server_config_from_entries(vec![entry("ok"), bad]).unwrap_err();
        assert_eq!(err.index, 1, "fails closed on the offending entry");
        assert!(matches!(
            err.kind,
            ToolConfigEntryErrorKind::NameOverrideInvalid { .. }
        ));
    }

    #[test]
    fn server_config_conversion_preserves_valid_name_overrides() {
        let mut a = entry("EchoAgentBuild:grep");
        a.name_override = Some("search".to_owned());
        let cfg = tool_server_config_from_entries(vec![a, entry("EchoAgentBuild:bash")]).unwrap();
        assert_eq!(cfg.tools.len(), 2);
        assert_eq!(cfg.tools[0].name_override.as_deref(), Some("search"));
        assert_eq!(cfg.tools[1].name_override, None);
    }

    #[test]
    fn server_config_conversion_maps_all_entries_with_default_preset() {
        let cfg = tool_server_config_from_entries(vec![entry("a"), entry("b")]).unwrap();
        assert_eq!(cfg.tools.len(), 2);
        assert_eq!(cfg.behavior_preset, None, "always the 'current' default");
    }

    #[test]
    fn server_config_conversion_fails_on_first_invalid_entry_with_index() {
        let mut bad = entry("bad");
        bad.params_json = Some("nope".to_owned());
        let err = tool_server_config_from_entries(vec![entry("ok"), bad]).unwrap_err();
        assert_eq!(err.index, 1);
        assert_eq!(err.tool_id, "bad");
    }
}
