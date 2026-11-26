use super::super::load::load_config_from_toml;
use super::super::mcp::{McpConfig, parse_mcp_config_with_oauth};
use super::*;
use toml::Value as TomlValue;
use toml::map::Map as TomlMap;
/// The `[toolset.ask_user_question]` settings write merges only that
/// sub-table: the toggled field lands, hand-written sibling keys survive,
/// and no other `[toolset]` defaults (bash/web_search) are splatted into
/// the user file. All-None leaves the file untouched.
#[test]
fn ask_user_question_merge_writes_subtable_without_splatting_toolset() {
    let root_val: TomlValue =
        toml::from_str("[toolset.ask_user_question]\ntimeout_secs = 30\n").unwrap();
    let mut root = root_val.as_table().unwrap().clone();
    let ask = crate::tools::config::AskUserQuestionToolConfig {
        timeout_enabled: Some(false),
        ..Default::default()
    };
    merge_ask_user_question_section(&mut root, &ask);
    let toolset = root.get("toolset").and_then(|v| v.as_table()).unwrap();
    assert_eq!(toolset.len(), 1, "only ask_user_question may be written");
    let ask_tbl = toolset
        .get("ask_user_question")
        .and_then(|v| v.as_table())
        .unwrap();
    assert_eq!(
        ask_tbl.get("timeout_enabled").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        ask_tbl.get("timeout_secs").and_then(|v| v.as_integer()),
        Some(30),
        "hand-written sibling keys must survive the merge"
    );
    let reparsed = load_config_from_toml(&TomlValue::Table(root.clone()));
    assert_eq!(reparsed.ask_user_question.timeout_enabled, Some(false));
    assert_eq!(reparsed.ask_user_question.timeout_secs, Some(30));
    let mut empty_root: TomlMap<String, TomlValue> = TomlMap::new();
    merge_ask_user_question_section(
        &mut empty_root,
        &crate::tools::config::AskUserQuestionToolConfig::default(),
    );
    assert!(
        empty_root.is_empty(),
        "all-None must not create an empty [toolset] header"
    );
    let mut scalar_root: TomlMap<String, TomlValue> = TomlMap::new();
    scalar_root.insert("toolset".into(), TomlValue::String("bogus".into()));
    merge_ask_user_question_section(&mut scalar_root, &ask);
    assert_eq!(
        scalar_root
            .get("toolset")
            .and_then(|v| v.get("ask_user_question"))
            .and_then(|a| a.get("timeout_enabled"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "scalar [toolset] must be replaced so the write lands"
    );
}
/// The `[telemetry]` write merges only `trace_upload`: hand-written sibling
/// telemetry keys survive, and an all-None config leaves the section alone.
#[test]
fn telemetry_merge_writes_trace_upload_without_splatting_section() {
    let root_val: TomlValue =
        toml::from_str("[telemetry]\ncustom_telemetry_flag = true\n").unwrap();
    let mut root = root_val.as_table().unwrap().clone();
    let telemetry = super::super::mcp::TelemetryPersistConfig {
        trace_upload: Some(true),
    };
    merge_section(&mut root, "telemetry", &telemetry);
    let section = root.get("telemetry").and_then(|v| v.as_table()).unwrap();
    assert_eq!(
        section.get("trace_upload").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        section
            .get("custom_telemetry_flag")
            .and_then(|v| v.as_bool()),
        Some(true),
        "hand-written sibling keys must survive the merge"
    );
    let reparsed = load_config_from_toml(&TomlValue::Table(root.clone()));
    assert_eq!(reparsed.telemetry.trace_upload, Some(true));
    let untouched_val: TomlValue = toml::from_str("[telemetry]\nevents_url = \"x\"\n").unwrap();
    let mut untouched = untouched_val.as_table().unwrap().clone();
    merge_section(
        &mut untouched,
        "telemetry",
        &super::super::mcp::TelemetryPersistConfig::default(),
    );
    assert_eq!(
        untouched
            .get("telemetry")
            .and_then(|v| v.get("events_url"))
            .and_then(|v| v.as_str()),
        Some("x"),
        "all-None must leave the existing section untouched"
    );
}
/// The `[features]` write merges only `feedback_trace_card`: hand-written
/// sibling feature keys survive, and an all-None config leaves the section
/// alone.
#[test]
fn features_merge_writes_feedback_trace_card_without_splatting_section() {
    let root_val: TomlValue = toml::from_str("[features]\nweb_fetch = true\n").unwrap();
    let mut root = root_val.as_table().unwrap().clone();
    let features = super::super::mcp::FeaturesPersistConfig {
        feedback_trace_card: Some(false),
    };
    merge_section(&mut root, "features", &features);
    let section = root.get("features").and_then(|v| v.as_table()).unwrap();
    assert_eq!(
        section.get("feedback_trace_card").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        section.get("web_fetch").and_then(|v| v.as_bool()),
        Some(true),
        "hand-written sibling keys must survive the merge"
    );
    let reparsed = load_config_from_toml(&TomlValue::Table(root.clone()));
    assert_eq!(reparsed.features.feedback_trace_card, Some(false));
    let untouched_val: TomlValue = toml::from_str("[features]\nvoice_mode = false\n").unwrap();
    let mut untouched = untouched_val.as_table().unwrap().clone();
    merge_section(
        &mut untouched,
        "features",
        &super::super::mcp::FeaturesPersistConfig::default(),
    );
    assert_eq!(
        untouched
            .get("features")
            .and_then(|v| v.get("voice_mode"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "all-None must leave the existing section untouched"
    );
}
#[test]
fn transport_oauth_client_id_takes_priority_over_block() {
    let json = r#"{
            "mcpServers": {
                "svc": {
                    "type": "http",
                    "url": "https://svc.example/mcp",
                    "oauth_client_id": "transport-client",
                    "oauth": { "clientId": "block-client" }
                }
            }
        }"#;
    let config: McpConfig = serde_json::from_str(json).expect("parse .mcp.json");
    let svc = config.mcp_servers.get("svc").unwrap();
    let oauth = svc.oauth_config().expect("oauth_config");
    assert_eq!(oauth.client_id.as_deref(), Some("transport-client"));
}
#[test]
fn parse_mcp_config_with_oauth_extracts_byo_client_id() {
    let json = r#"{
            "mcpServers": {
                "slack": {
                    "type": "http",
                    "url": "https://mcp.slack.example/mcp",
                    "oauth": { "clientId": "slack-byo-client" }
                },
                "plain": {
                    "type": "http",
                    "url": "https://plain.example/mcp"
                }
            }
        }"#;
    let config: McpConfig = serde_json::from_str(json).expect("parse .mcp.json");
    let (servers, oauth) = parse_mcp_config_with_oauth(&config, "test", &|s| s.to_string());
    assert_eq!(servers.len(), 2);
    assert_eq!(oauth.len(), 1);
    assert_eq!(
        oauth.get("slack").unwrap().client_id.as_deref(),
        Some("slack-byo-client")
    );
    assert!(!oauth.contains_key("plain"));
}
/// The merge recurses into nested tables and only ever inserts, so a key
/// inside `[ui.status_line]` that this build does not model is not at risk
/// from a settings write. The status-line parser relies on this: it reports
/// an unknown key rather than refusing to persist the section over it.
#[test]
fn merge_section_preserves_unmodeled_fields_inside_a_nested_table() {
    let mut table = TomlMap::new();
    let mut status_line = TomlMap::new();
    status_line.insert("type".into(), TomlValue::String("builtin".into()));
    status_line.insert("colour".into(), TomlValue::String("red".into()));
    let mut ui = TomlMap::new();
    ui.insert("status_line".into(), TomlValue::Table(status_line));
    table.insert("ui".into(), TomlValue::Table(ui));
    let cfg = crate::agent::config::UiConfig {
        status_line: echo_agent_status_line::test_support::StatusLineConfigFixture::from_kind(
            echo_agent_status_line::StatusLineType::Command,
        )
        .with_command("~/status_line.sh")
        .into_config(),
        ..Default::default()
    };
    merge_section(&mut table, "ui", &cfg);
    let written = table
        .get("ui")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("status_line"))
        .and_then(|v| v.as_table())
        .expect("the section survives");
    assert_eq!(
        written.get("colour").and_then(|v| v.as_str()),
        Some("red"),
        "a key this build does not model must survive a write of the ones it does"
    );
    assert_eq!(
        written.get("type").and_then(|v| v.as_str()),
        Some("command")
    );
}
#[test]
fn merge_section_preserves_unmodeled_fields() {
    let mut table = TomlMap::new();
    let mut ui = TomlMap::new();
    ui.insert("show_timestamps".into(), TomlValue::Boolean(true));
    ui.insert(
        "auto_dark_theme".into(),
        TomlValue::String("tokyonight".into()),
    );
    ui.insert("custom_user_key".into(), TomlValue::Integer(42));
    table.insert("ui".into(), TomlValue::Table(ui));
    let cfg = crate::agent::config::UiConfig::default();
    merge_section(&mut table, "ui", &cfg);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("show_timestamps").and_then(|v| v.as_bool()),
        Some(true),
        "pre-existing show_timestamps should survive merge with default struct"
    );
    assert_eq!(
        ui.get("auto_dark_theme").and_then(|v| v.as_str()),
        Some("tokyonight"),
        "pre-existing auto_dark_theme should survive merge with default struct"
    );
    assert_eq!(
        ui.get("custom_user_key").and_then(|v| v.as_integer()),
        Some(42),
        "truly unmodeled user-added key should survive merge"
    );
}
#[test]
fn merge_section_nested_display_refresh_preserves_future_knob() {
    let mut table = TomlMap::new();
    let mut ui = TomlMap::new();
    let mut dr = TomlMap::new();
    dr.insert("probe_enabled".into(), TomlValue::Boolean(true));
    dr.insert("future_knob".into(), TomlValue::Integer(42));
    ui.insert("display_refresh".into(), TomlValue::Table(dr));
    table.insert("ui".into(), TomlValue::Table(ui));
    let mut cfg = crate::agent::config::UiConfig::default();
    cfg.display_refresh.probe_enabled = Some(false);
    merge_section(&mut table, "ui", &cfg);
    let nested = table
        .get("ui")
        .and_then(|v| v.as_table())
        .and_then(|u| u.get("display_refresh"))
        .and_then(|v| v.as_table())
        .expect("display_refresh table");
    assert_eq!(
        nested.get("probe_enabled").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        nested.get("future_knob").and_then(|v| v.as_integer()),
        Some(42),
        "unknown nested keys must survive shallow-looking settings writes"
    );
}
#[test]
fn merge_section_updates_modeled_fields_preserving_unmodeled() {
    let mut table = TomlMap::new();
    let mut ui = TomlMap::new();
    ui.insert("yolo".into(), TomlValue::Boolean(false));
    ui.insert("show_timestamps".into(), TomlValue::Boolean(true));
    ui.insert(
        "auto_light_theme".into(),
        TomlValue::String("echoagentday".into()),
    );
    table.insert("ui".into(), TomlValue::Table(ui));
    let cfg = crate::agent::config::UiConfig {
        yolo: true,
        ..Default::default()
    };
    merge_section(&mut table, "ui", &cfg);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("yolo").and_then(|v| v.as_bool()),
        Some(true),
        "modeled field yolo should be updated"
    );
    assert_eq!(
        ui.get("show_timestamps").and_then(|v| v.as_bool()),
        Some(true),
        "pre-existing field not in serialized output should be preserved"
    );
    assert_eq!(
        ui.get("auto_light_theme").and_then(|v| v.as_str()),
        Some("echoagentday"),
        "pre-existing field not in serialized output should be preserved"
    );
}
#[test]
fn merge_section_creates_new_section() {
    let mut table = TomlMap::new();
    assert!(table.get("ui").is_none());
    let cfg = crate::agent::config::UiConfig {
        yolo: true,
        ..Default::default()
    };
    merge_section(&mut table, "ui", &cfg);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(ui.get("yolo").and_then(|v| v.as_bool()), Some(true));
}
/// Regression test: pager-side commits of a
/// [session] field (e.g., `auto_compact_threshold_percent`) must
/// NOT inject `load_envrc` into the user's config when the user
/// has never set it. Before the fix, `SessionConfig::load_envrc`
/// was plain `bool` with default `true` and no
/// `skip_serializing_if`, so EVERY pager save wrote
/// `[session].load_envrc = true` to disk — silently overriding any
/// managed-config `load_envrc = false` policy.
///
/// The fix widens `load_envrc` to `Option<bool>` with
/// `skip_serializing_if = "Option::is_none"`. After the fix, a
/// `SessionConfig::default()` (load_envrc: None,
/// auto_compact_threshold_percent: None) merges into a
/// pre-existing `[session]` table WITHOUT touching the user's
/// `load_envrc` key.
#[test]
fn merge_section_session_default_does_not_leak_load_envrc() {
    let mut table = TomlMap::new();
    assert!(table.get("session").is_none());
    let cfg = crate::agent::config::SessionConfig::default();
    merge_section(&mut table, "session", &cfg);
    if let Some(session) = table.get("session").and_then(|v| v.as_table()) {
        assert!(
            session.get("load_envrc").is_none(),
            "PR 12 R1 Bug 1: pager-side save with default SessionConfig must \
             NOT serialize load_envrc — that would override managed-config \
             policy. Found: {:?}",
            session.get("load_envrc"),
        );
        assert!(
            session.get("auto_compact_threshold_percent").is_none(),
            "default auto_compact_threshold_percent must not be serialized either"
        );
    }
}
/// Companion to the above: when the user explicitly commits a
/// non-default `auto_compact_threshold_percent`, the field is
/// serialized but `load_envrc` (still default None) is NOT.
/// Pins the asymmetry — committing one [session] field does not
/// "claim" the rest.
#[test]
fn merge_section_session_explicit_value_does_not_drag_load_envrc() {
    let mut table = TomlMap::new();
    let mut session = TomlMap::new();
    session.insert("load_envrc".into(), TomlValue::Boolean(false));
    table.insert("session".into(), TomlValue::Table(session));
    let cfg = crate::agent::config::SessionConfig {
        auto_compact_threshold_percent: Some(70),
        load_envrc: None,
    };
    merge_section(&mut table, "session", &cfg);
    let session = table.get("session").unwrap().as_table().unwrap();
    assert_eq!(
        session
            .get("auto_compact_threshold_percent")
            .and_then(|v| v.as_integer()),
        Some(70),
    );
    assert_eq!(
        session.get("load_envrc").and_then(|v| v.as_bool()),
        Some(false),
        "pre-existing load_envrc must survive a partial settings save"
    );
}
/// Follow-on: when the user DOES explicitly set
/// `load_envrc = false` via TOML, the value round-trips through
/// `load_config_from_toml` → mutate → `merge_section` correctly.
/// `None` means "absent on disk"; `Some(false)` means "user
/// explicitly disabled". The distinction must survive a save.
#[test]
fn session_load_envrc_explicit_false_round_trips() {
    let raw_config: TomlValue = toml::from_str(
        r#"
            [session]
            load_envrc = false
            "#,
    )
    .unwrap();
    let cfg = load_config_from_toml(&raw_config);
    assert_eq!(
        cfg.session.load_envrc,
        Some(false),
        "explicit load_envrc = false on disk must load as Some(false), not None"
    );
    let mut table = TomlMap::new();
    merge_section(&mut table, "session", &cfg.session);
    let session = table.get("session").unwrap().as_table().unwrap();
    assert_eq!(
        session.get("load_envrc").and_then(|v| v.as_bool()),
        Some(false),
        "explicit load_envrc = false must survive a save"
    );
}
#[test]
fn merge_section_empty_struct_preserves_existing_section() {
    let mut table = TomlMap::new();
    let mut harness = TomlMap::new();
    harness.insert("custom_key".into(), TomlValue::Boolean(true));
    harness.insert("another_key".into(), TomlValue::String("value".into()));
    table.insert("harness".into(), TomlValue::Table(harness));
    let cfg = crate::agent::config::HarnessConfig::default();
    merge_section(&mut table, "harness", &cfg);
    let harness = table.get("harness").unwrap().as_table().unwrap();
    assert_eq!(
        harness.get("custom_key").and_then(|v| v.as_bool()),
        Some(true),
        "existing fields must survive when struct serializes empty"
    );
    assert_eq!(
        harness.get("another_key").and_then(|v| v.as_str()),
        Some("value"),
    );
}
#[test]
fn ui_config_round_trip_preserves_pager_fields() {
    let toml_str = r#"
[ui]
yolo = true
show_timestamps = false
auto_dark_theme = "tokyonight"
auto_light_theme = "echoagentday"
"#;
    let root: TomlValue = toml::from_str(toml_str).unwrap();
    let cfg = load_config_from_toml(&root);
    assert!(cfg.ui.yolo);
    assert_eq!(cfg.ui.show_timestamps, Some(false));
    assert_eq!(cfg.ui.auto_dark_theme.as_deref(), Some("tokyonight"));
    assert_eq!(cfg.ui.auto_light_theme.as_deref(), Some("echoagentday"));
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "ui", &cfg.ui);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("show_timestamps").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        ui.get("auto_dark_theme").and_then(|v| v.as_str()),
        Some("tokyonight")
    );
    assert_eq!(
        ui.get("auto_light_theme").and_then(|v| v.as_str()),
        Some("echoagentday")
    );
    assert_eq!(ui.get("yolo").and_then(|v| v.as_bool()), Some(true));
}
#[test]
fn ui_config_hunk_tracker_mode_round_trips() {
    let root: TomlValue = toml::from_str("[ui]\nhunk_tracker_mode = \"off\"\n").unwrap();
    let cfg = load_config_from_toml(&root);
    assert_eq!(cfg.ui.hunk_tracker_mode.as_deref(), Some("off"));
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "ui", &cfg.ui);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("hunk_tracker_mode").and_then(|v| v.as_str()),
        Some("off")
    );
    let serialized = TomlValue::try_from(crate::agent::config::UiConfig::default()).unwrap();
    assert!(
        serialized
            .as_table()
            .unwrap()
            .get("hunk_tracker_mode")
            .is_none(),
        "hunk_tracker_mode=None must not appear in serialized output"
    );
}
#[test]
fn ui_config_serialization_behavior() {
    let cfg = crate::agent::config::UiConfig::default();
    let val = TomlValue::try_from(&cfg).unwrap();
    let table = val.as_table().unwrap();
    assert!(
        table.get("yolo").is_some(),
        "yolo must always serialize so revert-to-default persists"
    );
    assert!(
        table.get("compact_mode").is_some(),
        "compact_mode must always serialize so revert-to-default persists"
    );
    assert!(
        table.get("max_thoughts_width").is_some(),
        "max_thoughts_width must always serialize so revert-to-default persists"
    );
    assert!(
        table.get("show_timestamps").is_none(),
        "show_timestamps=None should not appear in serialized output"
    );
    assert!(
        table.get("auto_dark_theme").is_none(),
        "auto_dark_theme=None should not appear in serialized output"
    );
    assert!(
        table.get("theme").is_none(),
        "theme=None should not appear in serialized output"
    );
}
/// The settings-modal helpers in the parent module are 3-line
/// wrappers around `update_config(|cfg| cfg.ui.<field> = ...)`. To
/// guard against future drift between the wrapper and the schema
/// field, this test simulates each helper's closure against an
/// in-memory `Config` and asserts the field was set correctly. We
/// deliberately avoid disk I/O so the test stays hermetic.
///
/// The pattern mirrors exactly what `update_config` does internally:
/// `let mut cfg = load_config_from_toml(...); f(&mut cfg);`.
#[test]
fn merge_section_full_save_config_simulation() {
    let original = r#"
[ui]
show_timestamps = true
auto_dark_theme = "tokyonight"
auto_light_theme = "echoagentday"

[models]
default = "echo-agent-3"

[cli]
auto_update = true
"#;
    let root: TomlValue = toml::from_str(original).unwrap();
    let mut cfg = load_config_from_toml(&root);
    cfg.models.default = Some("echo-agent-4".to_string());
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "cli", &cfg.cli);
    merge_section(&mut table, "models", &cfg.models);
    merge_section(&mut table, "ui", &cfg.ui);
    merge_section(&mut table, "harness", &cfg.harness);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("show_timestamps").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        ui.get("auto_dark_theme").and_then(|v| v.as_str()),
        Some("tokyonight")
    );
    assert_eq!(
        ui.get("auto_light_theme").and_then(|v| v.as_str()),
        Some("echoagentday")
    );
    let models = table.get("models").unwrap().as_table().unwrap();
    assert_eq!(
        models.get("default").and_then(|v| v.as_str()),
        Some("echo-agent-4")
    );
}
#[test]
fn merge_section_revert_to_default_overwrites_old_value() {
    let mut table = TomlMap::new();
    let mut ui = TomlMap::new();
    ui.insert("yolo".into(), TomlValue::Boolean(true));
    ui.insert("compact_mode".into(), TomlValue::Boolean(true));
    table.insert("ui".into(), TomlValue::Table(ui));
    let cfg = crate::agent::config::UiConfig::default();
    merge_section(&mut table, "ui", &cfg);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("yolo").and_then(|v| v.as_bool()),
        Some(false),
        "yolo=false must overwrite the old yolo=true"
    );
    assert_eq!(
        ui.get("compact_mode").and_then(|v| v.as_bool()),
        Some(false),
        "compact_mode=false must overwrite the old compact_mode=true"
    );
}
#[test]
fn merge_section_replaces_non_table_section() {
    let mut table = TomlMap::new();
    table.insert("ui".into(), TomlValue::String("garbage".into()));
    let cfg = crate::agent::config::UiConfig {
        yolo: true,
        ..Default::default()
    };
    merge_section(&mut table, "ui", &cfg);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("yolo").and_then(|v| v.as_bool()),
        Some(true),
        "non-table section should be replaced with proper table"
    );
}
#[test]
fn models_config_serializes_only_some_fields() {
    let m = crate::agent::config::ModelsConfig {
        default: Some("echo-agent-3".to_string()),
        ..Default::default()
    };
    let v = TomlValue::try_from(&m).expect("serialize ModelsConfig");
    if let TomlValue::Table(t) = v {
        assert_eq!(t.len(), 1);
        assert!(t.contains_key("default"));
        assert!(!t.contains_key("web_search"));
        assert!(!t.contains_key("session_summary"));
        assert!(!t.contains_key("image_description"));
        assert!(!t.contains_key("hidden_models"));
        assert!(!t.contains_key("disabled_models"));
        assert!(!t.contains_key("allowed_models"));
        assert!(!t.contains_key("agent_type"));
        assert_eq!(
            t.get("default").and_then(|x| x.as_str()),
            Some("echo-agent-3")
        );
    } else {
        panic!("expected table from serialization");
    }
}
/// Canonical list of every `Option<T>` field in [`CliConfig`].  Kept in one
/// place so both serialization and merge-section tests automatically cover
/// newly-added fields without copy-pasting assertion lists.
const CLI_CONFIG_OPTION_FIELDS: &[&str] = &[
    "auto_update",
    "dismissed_version",
    "installer",
    "npm_registry",
    "channel",
    "use_leader",
    "show_tips",
    "worktree_type",
    "session_registry",
    "minimum_version",
    "maximum_version",
    "required_minimum_version",
    "required_maximum_version",
];
/// Assert that every `CliConfig` `Option<T>` field NOT in `present` is
/// absent from `table`.
fn assert_cli_option_fields_absent(table: &TomlMap<String, TomlValue>, present: &[&str]) {
    for field in CLI_CONFIG_OPTION_FIELDS {
        if !present.contains(field) {
            assert!(
                !table.contains_key(*field),
                "expected Option field `{field}` to be absent when not set",
            );
        }
    }
}
#[test]
fn cli_config_serializes_only_some_fields() {
    let c = crate::agent::config::CliConfig {
        auto_update: Some(true),
        channel: Some("beta".to_string()),
        ..Default::default()
    };
    let v = TomlValue::try_from(&c).expect("serialize CliConfig");
    if let TomlValue::Table(t) = v {
        assert_eq!(t.len(), 2);
        assert!(t.contains_key("auto_update"));
        assert!(t.contains_key("channel"));
        assert_cli_option_fields_absent(&t, &["auto_update", "channel"]);
        assert_eq!(t.get("auto_update").and_then(|x| x.as_bool()), Some(true));
        assert_eq!(t.get("channel").and_then(|x| x.as_str()), Some("beta"));
    } else {
        panic!("expected table from serialization");
    }
}
#[test]
fn merge_section_cli_only_updates_set_fields_preserves_unmodeled() {
    let mut table = TomlMap::new();
    let mut cli = TomlMap::new();
    cli.insert("use_leader".into(), TomlValue::Boolean(true));
    cli.insert("show_tips".into(), TomlValue::Boolean(false));
    cli.insert(
        "custom_pager_key".into(),
        TomlValue::String("keep-this".into()),
    );
    table.insert("cli".into(), TomlValue::Table(cli));
    let cfg = crate::agent::config::CliConfig {
        auto_update: Some(false),
        dismissed_version: Some("v1.2.3".to_string()),
        ..Default::default()
    };
    merge_section(&mut table, "cli", &cfg);
    let c = table.get("cli").unwrap().as_table().unwrap();
    assert_eq!(c.get("auto_update").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        c.get("dismissed_version").and_then(|v| v.as_str()),
        Some("v1.2.3")
    );
    assert_eq!(c.get("use_leader").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(c.get("show_tips").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        c.get("custom_pager_key").and_then(|v| v.as_str()),
        Some("keep-this")
    );
    assert_cli_option_fields_absent(
        c,
        &[
            "auto_update",
            "dismissed_version",
            "use_leader",
            "show_tips",
        ],
    );
}
#[test]
fn merge_section_models_only_updates_set_fields_preserves_others() {
    let mut table = TomlMap::new();
    let mut models = TomlMap::new();
    models.insert("web_search".into(), TomlValue::String("old-search".into()));
    models.insert("unmodeled_foo".into(), TomlValue::String("keep-me".into()));
    table.insert("models".into(), TomlValue::Table(models));
    let cfg = crate::agent::config::ModelsConfig {
        default: Some("echo-agent-new".to_string()),
        ..Default::default()
    };
    merge_section(&mut table, "models", &cfg);
    let m = table.get("models").unwrap().as_table().unwrap();
    assert_eq!(
        m.get("default").and_then(|v| v.as_str()),
        Some("echo-agent-new")
    );
    assert_eq!(
        m.get("web_search").and_then(|v| v.as_str()),
        Some("old-search")
    );
    assert_eq!(
        m.get("unmodeled_foo").and_then(|v| v.as_str()),
        Some("keep-me")
    );
    assert!(!m.contains_key("session_summary"));
}
#[test]
fn persist_preferred_model_flow_roundtrips_via_load_and_new_from_toml_cfg() {
    let original = "[models]\ndefault = \"echo-agent-old\"\nweb_search = \"some-search\"\n";
    let root: TomlValue = toml::from_str(original).unwrap();
    let mut cfg = load_config_from_toml(&root);
    cfg.models.default = Some("echo-agent-persisted".to_string());
    let mut table = if let TomlValue::Table(t) = root {
        t
    } else {
        TomlMap::new()
    };
    merge_section(&mut table, "models", &cfg.models);
    let reloaded_root = TomlValue::Table(table);
    let reloaded = load_config_from_toml(&reloaded_root);
    assert_eq!(
        reloaded.models.default.as_deref(),
        Some("echo-agent-persisted")
    );
    let cfg2 =
        crate::agent::config::Config::new_from_toml_cfg(&reloaded_root).expect("new_from_toml_cfg");
    assert_eq!(cfg2.models.default.as_deref(), Some("echo-agent-persisted"));
}
#[test]
fn merge_section_cli_show_tips_writes_under_cli_section() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::CliConfig {
        show_tips: Some(false),
        ..Default::default()
    };
    merge_section(&mut table, "cli", &cfg);
    let c = table.get("cli").unwrap().as_table().unwrap();
    assert_eq!(
        c.get("show_tips").and_then(|v| v.as_bool()),
        Some(false),
        "set_show_tips must persist Some(false) at `[cli].show_tips`"
    );
}
#[test]
fn merge_section_cli_show_tips_none_does_not_serialize() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::CliConfig::default();
    assert!(cfg.show_tips.is_none());
    merge_section(&mut table, "cli", &cfg);
    if let Some(c) = table.get("cli").and_then(|v| v.as_table()) {
        assert!(
            c.get("show_tips").is_none(),
            "default show_tips: None must not serialize — \
             managed-config layering depends on absent-means-defer"
        );
    }
}
#[test]
fn merge_section_cli_session_picker_grouped_writes_under_cli_section() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::CliConfig {
        session_picker_grouped: Some(false),
        ..Default::default()
    };
    merge_section(&mut table, "cli", &cfg);
    let c = table.get("cli").unwrap().as_table().unwrap();
    assert_eq!(
        c.get("session_picker_grouped").and_then(|v| v.as_bool()),
        Some(false),
        "Some(false) must round-trip to `[cli].session_picker_grouped`"
    );
}
#[test]
fn merge_section_cli_auto_update_writes_under_cli_section() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::CliConfig {
        auto_update: Some(false),
        ..Default::default()
    };
    merge_section(&mut table, "cli", &cfg);
    let c = table.get("cli").unwrap().as_table().unwrap();
    assert_eq!(
        c.get("auto_update").and_then(|v| v.as_bool()),
        Some(false),
        "set_auto_update must persist Some(false) at `[cli].auto_update`"
    );
}
#[test]
fn merge_section_cli_use_leader_writes_under_cli_section() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::CliConfig {
        use_leader: Some(true),
        ..Default::default()
    };
    merge_section(&mut table, "cli", &cfg);
    let c = table.get("cli").unwrap().as_table().unwrap();
    assert_eq!(
        c.get("use_leader").and_then(|v| v.as_bool()),
        Some(true),
        "Some(true) must round-trip to `[cli].use_leader`"
    );
}
/// Verify `Option<bool>` + `skip_serializing_if` prevents one
/// `[session]` field from dragging unrelated fields.
#[test]
fn merge_section_session_load_envrc_writes_under_session_section() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::SessionConfig {
        load_envrc: Some(false),
        ..Default::default()
    };
    merge_section(&mut table, "session", &cfg);
    let s = table.get("session").unwrap().as_table().unwrap();
    assert_eq!(
        s.get("load_envrc").and_then(|v| v.as_bool()),
        Some(false),
        "Some(false) must round-trip to `[session].load_envrc`"
    );
}
/// Committing `load_envrc` alone must not inject `auto_compact_threshold_percent`.
#[test]
fn merge_section_session_load_envrc_does_not_drag_auto_compact() {
    let mut table = TomlMap::new();
    let cfg = crate::agent::config::SessionConfig {
        load_envrc: Some(true),
        auto_compact_threshold_percent: None,
    };
    merge_section(&mut table, "session", &cfg);
    let s = table.get("session").unwrap().as_table().unwrap();
    assert_eq!(s.get("load_envrc").and_then(|v| v.as_bool()), Some(true));
    assert!(
        s.get("auto_compact_threshold_percent").is_none(),
        "default auto_compact_threshold_percent: None must not serialize \
         when only load_envrc is being committed"
    );
}
mod resolve_auto_compact {
    use super::super::super::RemoteSettings;
    use super::super::super::resolve::{
        DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT, ENV_AUTO_COMPACT_THRESHOLD_PERCENT,
        resolve_auto_compact_threshold_percent,
    };
    use crate::agent::config::{Config, ConfigModelOverride, ModelInfo};
    use std::sync::Mutex;
    const TEST_MODEL: &str = "echo-agent-4.5";
    const OTHER_MODEL: &str = "echo-agent-4.3";
    /// Serialize tests that mutate `ECHO_AGENT_AUTO_COMPACT_THRESHOLD_PERCENT`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    /// Build a `Config` populated with optional per-source values for the
    /// `TEST_MODEL`. Any `None` argument means "that source is unset".
    fn make_cfg(
        user_session: Option<u8>,
        user_per_model: Option<u8>,
        gb_global: Option<u8>,
    ) -> Config {
        let mut cfg = Config::default();
        cfg.session.auto_compact_threshold_percent = user_session;
        if let Some(v) = user_per_model {
            cfg.config_models.insert(
                TEST_MODEL.to_owned(),
                ConfigModelOverride {
                    auto_compact_threshold_percent: Some(v),
                    ..ConfigModelOverride::default()
                },
            );
        }
        if let Some(v) = gb_global {
            cfg.remote_settings = Some(RemoteSettings {
                auto_compact_threshold_percent: Some(v),
                ..RemoteSettings::default()
            });
        }
        cfg
    }
    /// ModelInfo populated with the GB per-model value (or none).
    fn model_info(gb_per_model: Option<u8>) -> ModelInfo {
        let mut info = ModelInfo::fallback(TEST_MODEL);
        info.auto_compact_threshold_percent = gb_per_model;
        info
    }
    /// Run the resolver against the assembled inputs.
    fn resolve(cfg: &Config, gb_per_model: Option<u8>) -> u8 {
        let info = model_info(gb_per_model);
        resolve_auto_compact_threshold_percent(cfg, TEST_MODEL, Some(&info))
    }
    /// RAII guard that swaps the env var for the duration of a test and
    /// restores the previous value on drop. Acquires `ENV_LOCK` so two
    /// env-var tests never run concurrently.
    struct EnvVarGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }
    impl EnvVarGuard {
        fn set(value: &str) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT).ok();
            unsafe { std::env::set_var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT, value) };
            Self { _lock: lock, prev }
        }
        fn unset() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prev = std::env::var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT).ok();
            unsafe { std::env::remove_var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT) };
            Self { _lock: lock, prev }
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT, v) },
                None => unsafe { std::env::remove_var(ENV_AUTO_COMPACT_THRESHOLD_PERCENT) },
            }
        }
    }
    #[test]
    fn all_unset_returns_default_85() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, None, None);
        assert_eq!(resolve(&cfg, None), DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT);
    }
    #[test]
    fn all_unset_no_model_info_returns_default_85() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, None, None);
        assert_eq!(
            resolve_auto_compact_threshold_percent(&cfg, TEST_MODEL, None),
            DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT
        );
    }
    #[test]
    fn gb_global_only() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, None, Some(40));
        assert_eq!(resolve(&cfg, None), 40);
    }
    #[test]
    fn gb_per_model_beats_gb_global() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, None, Some(40));
        assert_eq!(resolve(&cfg, Some(90)), 90);
    }
    #[test]
    fn user_session_beats_gb_per_model() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(Some(75), None, None);
        assert_eq!(resolve(&cfg, Some(90)), 75);
    }
    #[test]
    fn user_session_beats_gb_global() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(Some(75), None, Some(40));
        assert_eq!(resolve(&cfg, None), 75);
    }
    #[test]
    fn user_per_model_beats_user_session() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(Some(75), Some(70), None);
        assert_eq!(resolve(&cfg, None), 70);
    }
    #[test]
    fn user_per_model_beats_gb_per_model() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, Some(70), None);
        assert_eq!(resolve(&cfg, Some(90)), 70);
    }
    #[test]
    fn user_per_model_beats_gb_global() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, Some(70), Some(40));
        assert_eq!(resolve(&cfg, None), 70);
    }
    #[test]
    fn user_per_model_beats_everything_below_env() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(Some(75), Some(70), Some(40));
        assert_eq!(resolve(&cfg, Some(90)), 70);
    }
    #[test]
    fn env_beats_user_per_model() {
        let _g = EnvVarGuard::set("50");
        let cfg = make_cfg(Some(75), Some(70), Some(40));
        assert_eq!(resolve(&cfg, Some(90)), 50);
    }
    #[test]
    fn env_at_lower_bound_is_honored() {
        let _g = EnvVarGuard::set("0");
        let cfg = make_cfg(Some(75), None, None);
        assert_eq!(resolve(&cfg, None), 0);
    }
    #[test]
    fn env_at_upper_bound_is_honored() {
        let _g = EnvVarGuard::set("100");
        let cfg = make_cfg(Some(75), None, None);
        assert_eq!(resolve(&cfg, None), 100);
    }
    #[test]
    fn env_out_of_range_high_falls_through() {
        let _g = EnvVarGuard::set("101");
        let cfg = make_cfg(Some(75), None, None);
        assert_eq!(resolve(&cfg, None), 75);
    }
    #[test]
    fn env_out_of_range_negative_falls_through() {
        let _g = EnvVarGuard::set("-1");
        let cfg = make_cfg(Some(75), None, None);
        assert_eq!(resolve(&cfg, None), 75);
    }
    #[test]
    fn env_unparseable_falls_through() {
        let _g = EnvVarGuard::set("not-a-number");
        let cfg = make_cfg(Some(75), None, None);
        assert_eq!(resolve(&cfg, None), 75);
    }
    #[test]
    fn env_empty_falls_through_to_default() {
        let _g = EnvVarGuard::set("");
        let cfg = make_cfg(None, None, None);
        assert_eq!(resolve(&cfg, None), DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT);
    }
    #[test]
    fn user_per_model_for_other_model_does_not_match() {
        let _g = EnvVarGuard::unset();
        let mut cfg = Config::default();
        cfg.session.auto_compact_threshold_percent = None;
        cfg.config_models.insert(
            OTHER_MODEL.to_owned(),
            ConfigModelOverride {
                auto_compact_threshold_percent: Some(70),
                ..ConfigModelOverride::default()
            },
        );
        assert_eq!(resolve(&cfg, None), DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT);
    }
    #[test]
    fn user_per_model_for_other_model_falls_through_to_user_session() {
        let _g = EnvVarGuard::unset();
        let mut cfg = Config::default();
        cfg.session.auto_compact_threshold_percent = Some(75);
        cfg.config_models.insert(
            OTHER_MODEL.to_owned(),
            ConfigModelOverride {
                auto_compact_threshold_percent: Some(70),
                ..ConfigModelOverride::default()
            },
        );
        assert_eq!(resolve(&cfg, None), 75);
    }
    #[test]
    fn missing_model_info_falls_through_to_gb_global() {
        let _g = EnvVarGuard::unset();
        let cfg = make_cfg(None, None, Some(40));
        assert_eq!(
            resolve_auto_compact_threshold_percent(&cfg, TEST_MODEL, None),
            40
        );
    }
    #[test]
    fn no_remote_settings_falls_through_to_default() {
        let _g = EnvVarGuard::unset();
        let cfg = Config {
            remote_settings: None,
            ..Config::default()
        };
        assert_eq!(resolve(&cfg, None), DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT);
    }
    #[test]
    fn apply_does_not_merge_auto_compact_threshold_percent_into_model_info() {
        use crate::agent::config::{EndpointsConfig, ModelEntry};
        let endpoints = EndpointsConfig::default();
        let base = ModelEntry::fallback(TEST_MODEL, &endpoints);
        let over = ConfigModelOverride {
            auto_compact_threshold_percent: Some(42),
            ..ConfigModelOverride::default()
        };
        let merged = over.apply(TEST_MODEL, Some(base), &endpoints);
        assert_eq!(
            merged.info.auto_compact_threshold_percent, None,
            "ConfigModelOverride::apply must NOT merge `auto_compact_threshold_percent` \
             into ModelInfo — the resolver depends on the field staying empty so \
             user-per-model and GB-per-model remain distinguishable tiers"
        );
    }
}
#[test]
fn settings_helpers_target_correct_ui_fields() {
    fn apply<F: FnOnce(&mut Config)>(f: F) -> Config {
        let mut cfg = load_config_from_toml(&TomlValue::Table(TomlMap::new()));
        f(&mut cfg);
        cfg
    }
    let cfg = apply(|cfg| cfg.ui.compact_mode = true);
    assert!(cfg.ui.compact_mode, "set_compact_mode must set bool field");
    let cfg = apply(|cfg| cfg.ui.compact_mode = false);
    assert!(!cfg.ui.compact_mode);
    let cfg = apply(|cfg| cfg.ui.show_timestamps = Some(true));
    assert_eq!(cfg.ui.show_timestamps, Some(true));
    let cfg = apply(|cfg| cfg.ui.show_timestamps = Some(false));
    assert_eq!(cfg.ui.show_timestamps, Some(false));
    let cfg = apply(|cfg| cfg.ui.simple_mode = Some(true));
    assert_eq!(cfg.ui.simple_mode, Some(true));
    let cfg = apply(|cfg| cfg.ui.simple_mode = Some(false));
    assert_eq!(cfg.ui.simple_mode, Some(false));
    let cfg = apply(|cfg| cfg.ui.theme = Some("tokyonight".to_string()));
    assert_eq!(cfg.ui.theme, Some("tokyonight".to_string()));
    let cfg = apply(|cfg| cfg.ui.theme = Some("auto".to_string()));
    assert_eq!(cfg.ui.theme, Some("auto".to_string()));
    let cfg = apply(|cfg| cfg.ui.auto_dark_theme = Some("tokyonight".to_string()));
    assert_eq!(cfg.ui.auto_dark_theme, Some("tokyonight".to_string()));
    let cfg = apply(|cfg| cfg.ui.auto_light_theme = Some("echoagentday".to_string()));
    assert_eq!(cfg.ui.auto_light_theme, Some("echoagentday".to_string()));
    let cfg = apply(|cfg| cfg.ui.hunk_tracker_mode = Some("off".to_string()));
    assert_eq!(cfg.ui.hunk_tracker_mode, Some("off".to_string()));
    let cfg = apply(|cfg| cfg.ui.screen_mode = Some("minimal".to_string()));
    assert_eq!(cfg.ui.screen_mode, Some("minimal".to_string()));
    let cfg = apply(|cfg| cfg.ui.screen_mode = Some("fullscreen".to_string()));
    assert_eq!(cfg.ui.screen_mode, Some("fullscreen".to_string()));
}
/// Theme merge round-trip: verifies the theme field is set and
/// unmodeled fields survive. Same pattern as `set_compact_mode_round_trips`.
#[test]
fn set_theme_round_trips_through_merge() {
    let original = r#"
[ui]
compact_mode = true
theme = "echoagentnight"
auto_dark_theme = "tokyonight"
custom_user_key = "preserve-me"
"#;
    let root: TomlValue = toml::from_str(original).unwrap();
    let mut cfg = load_config_from_toml(&root);
    cfg.ui.theme = Some("tokyonight".to_string());
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "ui", &cfg.ui);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("theme").and_then(|v| v.as_str()),
        Some("tokyonight"),
        "theme must be set"
    );
    assert_eq!(
        ui.get("compact_mode").and_then(|v| v.as_bool()),
        Some(true),
        "unrelated modeled field must survive"
    );
    assert_eq!(
        ui.get("auto_dark_theme").and_then(|v| v.as_str()),
        Some("tokyonight"),
        "auto_dark_theme must survive"
    );
    assert_eq!(
        ui.get("custom_user_key").and_then(|v| v.as_str()),
        Some("preserve-me"),
        "unmodeled field must survive"
    );
}
/// Same as above but for `set_auto_dark_theme` and `set_auto_light_theme`.
#[test]
fn set_auto_dark_and_light_theme_round_trip_through_merge() {
    let original = r#"
[ui]
theme = "auto"
auto_dark_theme = "echoagentnight"
auto_light_theme = "echoagentday"
custom_unknown_key = 42
"#;
    let root: TomlValue = toml::from_str(original).unwrap();
    let mut cfg = load_config_from_toml(&root);
    cfg.ui.auto_dark_theme = Some("tokyonight".to_string());
    cfg.ui.auto_light_theme = Some("rosepine-moon".to_string());
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "ui", &cfg.ui);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("auto_dark_theme").and_then(|v| v.as_str()),
        Some("tokyonight"),
    );
    assert_eq!(
        ui.get("auto_light_theme").and_then(|v| v.as_str()),
        Some("rosepine-moon"),
    );
    assert_eq!(
        ui.get("theme").and_then(|v| v.as_str()),
        Some("auto"),
        "theme=auto must survive"
    );
    assert_eq!(
        ui.get("custom_unknown_key").and_then(|v| v.as_integer()),
        Some(42),
        "unmodeled field must survive"
    );
}
/// Compact-mode merge round-trip: flipped field persists,
/// unrelated modeled and unmodeled fields survive.
#[test]
fn set_compact_mode_round_trips_through_merge() {
    let original = r#"
[ui]
compact_mode = false
auto_dark_theme = "tokyonight"
show_timestamps = true
custom_user_key = "preserve-me"
"#;
    let root: TomlValue = toml::from_str(original).unwrap();
    let mut cfg = load_config_from_toml(&root);
    cfg.ui.compact_mode = true;
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "ui", &cfg.ui);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("compact_mode").and_then(|v| v.as_bool()),
        Some(true),
        "compact_mode must be flipped"
    );
    assert_eq!(
        ui.get("auto_dark_theme").and_then(|v| v.as_str()),
        Some("tokyonight"),
        "unrelated UI field must survive"
    );
    assert_eq!(
        ui.get("show_timestamps").and_then(|v| v.as_bool()),
        Some(true),
        "show_timestamps must survive"
    );
    assert_eq!(
        ui.get("custom_user_key").and_then(|v| v.as_str()),
        Some("preserve-me"),
        "unmodeled (unknown to the UiConfig schema) field must survive — \
         this is the merge_section invariant the new helpers depend on"
    );
}
/// Same merge round-trip for `show_timestamps` and `simple_mode`.
#[test]
fn set_show_timestamps_and_simple_mode_round_trip_through_merge() {
    let original = r#"
[ui]
compact_mode = true
custom_unknown_key = 42
"#;
    let root: TomlValue = toml::from_str(original).unwrap();
    let mut cfg = load_config_from_toml(&root);
    cfg.ui.show_timestamps = Some(false);
    cfg.ui.simple_mode = Some(false);
    let mut table = root.as_table().unwrap().clone();
    merge_section(&mut table, "ui", &cfg.ui);
    let ui = table.get("ui").unwrap().as_table().unwrap();
    assert_eq!(
        ui.get("show_timestamps").and_then(|v| v.as_bool()),
        Some(false),
    );
    assert_eq!(ui.get("simple_mode").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        ui.get("compact_mode").and_then(|v| v.as_bool()),
        Some(true),
        "unrelated modeled field must survive"
    );
    assert_eq!(
        ui.get("custom_unknown_key").and_then(|v| v.as_integer()),
        Some(42),
        "unmodeled (unknown to the schema) field must survive"
    );
}

/// The advisory transaction lock must span the read as well as the replace.
/// Otherwise every thread below can read the same snapshot and only the last
/// writer survives. Unrelated hand-written sections are part of the assertion
/// because the desktop and Runtime own different slices of this same file.
#[test]
fn config_toml_transaction_serializes_concurrent_writers_and_preserves_fields() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[custom]
keep = "hand-written"

[models]
default = "existing-model"
"#,
    )
    .unwrap();

    const WRITERS: usize = 12;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
    let handles = (0..WRITERS)
        .map(|index| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                update_config_toml_at(&path, |root| {
                    let writers = root
                        .as_table_mut()
                        .unwrap()
                        .entry("concurrent_writers")
                        .or_insert_with(|| TomlValue::Table(TomlMap::new()))
                        .as_table_mut()
                        .unwrap();
                    writers.insert(index.to_string(), TomlValue::Integer(index as i64));
                    // Widen the stale-snapshot window this test protects.
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    Ok(())
                })
                .unwrap();
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let saved: TomlValue = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        saved
            .get("concurrent_writers")
            .and_then(TomlValue::as_table)
            .map(TomlMap::len),
        Some(WRITERS),
        "no concurrent transaction may be lost"
    );
    assert_eq!(
        saved
            .get("custom")
            .and_then(|value| value.get("keep"))
            .and_then(TomlValue::as_str),
        Some("hand-written")
    );
    assert_eq!(
        saved
            .get("models")
            .and_then(|value| value.get("default"))
            .and_then(TomlValue::as_str),
        Some("existing-model")
    );
}

#[test]
fn config_toml_transaction_rejects_oversized_input_without_mutating_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let original = vec![b'#'; MAX_CONFIG_FILE_BYTES as usize + 1];
    std::fs::write(&path, &original).unwrap();

    let error = update_config_toml_at(&path, |_| Ok(())).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::InvalidData)
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn config_toml_transaction_rejects_malformed_input_without_mutating_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let original = b"[broken\nvalue = true\n";
    std::fs::write(&path, original).unwrap();

    let error = update_config_toml_at(&path, |_| Ok(())).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("refusing to overwrite unparseable"),
        "unexpected error: {error:#}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn config_toml_transaction_rejects_symlink_without_touching_target() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("outside.toml");
    let path = temp.path().join("config.toml");
    std::fs::write(&target, "secret = true\n").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();

    let error = update_config_toml_at(&path, |_| Ok(())).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::InvalidData)
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "secret = true\n");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn atomic_config_write_rejects_non_file_destination() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    std::fs::create_dir(&path).unwrap();

    let error = atomic_write_string(&path, "valid = true\n").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(path.is_dir());
}

#[cfg(unix)]
#[test]
fn atomic_config_write_rejects_symlink_parent() {
    let temp = tempfile::tempdir().unwrap();
    let real_parent = temp.path().join("real-parent");
    let linked_parent = temp.path().join("linked-parent");
    std::fs::create_dir(&real_parent).unwrap();
    std::os::unix::fs::symlink(&real_parent, &linked_parent).unwrap();

    let error =
        atomic_write_string(&linked_parent.join("config.toml"), "valid = true\n").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(!real_parent.join("config.toml").exists());
}
