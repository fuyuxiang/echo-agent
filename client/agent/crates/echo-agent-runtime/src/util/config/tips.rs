use serde::Deserialize;
use toml::Value as TomlValue;

/// Read `[cli] show_tips` from config.toml. Returns `None` if not set.
/// When `Some(false)`, the tip-of-the-day is suppressed on startup.
pub(crate) fn show_tips_from_toml_opt(root: &TomlValue) -> Option<bool> {
    if let TomlValue::Table(table) = root
        && let Some(TomlValue::Table(cli)) = table.get("cli")
    {
        cli.get("show_tips").and_then(|v| v.as_bool())
    } else {
        None
    }
}
/// Local `[tips]` config section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TipsOverride {
    pub tips: Vec<String>,
    /// When true, drop remote/default tips entirely.
    pub exclude_default: bool,
}

/// Parse `[tips]` from a TOML value.
pub(crate) fn tips_from_toml(root: &TomlValue) -> Option<TipsOverride> {
    root.get("tips")?.clone().try_into::<TipsOverride>().ok()
}

/// Merge tip sources in priority order.
///
/// If any local source sets `exclude_default = true`, remote tips are dropped entirely.
/// Otherwise remote tips are inserted after requirements and before user/managed config.
pub(crate) fn merge_tips(
    requirements: Option<TipsOverride>,
    user: Option<TipsOverride>,
    managed: Option<TipsOverride>,
    remote_tips: Option<&[String]>,
) -> Vec<String> {
    let exclude = [&requirements, &user, &managed]
        .into_iter()
        .flatten()
        .any(|s| s.exclude_default);

    let mut out = Vec::new();
    if let Some(src) = requirements.as_ref() {
        out.extend(src.tips.iter().cloned());
    }
    if !exclude && let Some(remote) = remote_tips {
        out.extend(remote.iter().cloned());
    }
    if let Some(src) = user.as_ref() {
        out.extend(src.tips.iter().cloned());
    }
    if let Some(src) = managed.as_ref() {
        out.extend(src.tips.iter().cloned());
    }
    out
}

/// Resolve the merged tip list from pre-loaded config layers.
///
/// Priority: requirements > remote > user config > managed config.
/// `ECHO_AGENT_TIPS_OVERRIDE` env var overrides everything (debug builds only).
/// `[cli] show_tips = false` in requirements or user config kills all tips.
pub fn resolve_tips(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote_tips: Option<&[String]>,
) -> Vec<String> {
    if requirements.and_then(show_tips_from_toml_opt) == Some(false) {
        return Vec::new();
    }
    if user.and_then(show_tips_from_toml_opt) == Some(false) {
        return Vec::new();
    }

    #[cfg(debug_assertions)]
    if let Ok(raw) = std::env::var("ECHO_AGENT_TIPS_OVERRIDE") {
        return raw.split('|').map(str::to_string).collect();
    }

    let req = requirements.and_then(tips_from_toml);
    let usr = user.and_then(tips_from_toml);
    let mgd = managed.and_then(tips_from_toml);

    // Priority: requirements > remote > user > managed.
    merge_tips(req, usr, mgd, remote_tips)
}

pub const SLASH_COMMAND_TAGS_CONFIG_PATH: &str = "slash_command_tags";

/// Parse `[slash_command_tags]` from a TOML value into a name → tag map.
/// Only string values are kept; non-string entries are ignored.
fn slash_command_tags_from_toml(root: &TomlValue) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Some(TomlValue::Table(table)) = root.get(SLASH_COMMAND_TAGS_CONFIG_PATH) {
        for (name, value) in table {
            if let Some(tag) = value.as_str() {
                out.insert(name.clone(), tag.to_string());
            }
        }
    }
    out
}

/// Parse a `ECHO_AGENT_SLASH_COMMAND_TAGS` payload (a JSON object of string→string)
/// into a name → tag map. `None`/empty → empty; malformed → warn + empty. Split
/// from env-reading so the parse is unit-testable without mutating process env.
fn parse_slash_command_tags_json(raw: Option<&str>) -> std::collections::HashMap<String, String> {
    // Unset or empty/whitespace-only is the normal "no override" state, not an
    // error — only real, non-empty input is parsed (and warned on failure).
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return std::collections::HashMap::new();
    };
    match serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw) {
        Ok(map) => map.into_iter().collect(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "ignoring malformed ECHO_AGENT_SLASH_COMMAND_TAGS; expected a JSON object of string values"
            );
            std::collections::HashMap::new()
        }
    }
}

/// Read per-command tags from the `ECHO_AGENT_SLASH_COMMAND_TAGS` env var. Unset →
/// empty; malformed → warn + empty.
fn slash_command_tags_from_env() -> std::collections::HashMap<String, String> {
    parse_slash_command_tags_json(
        std::env::var("ECHO_AGENT_SLASH_COMMAND_TAGS")
            .ok()
            .as_deref(),
    )
}

/// Pure per-key merge of the three tag sources. Precedence lowest → highest:
/// remote (base) → local `[slash_command_tags]` → env. Every key from every
/// layer survives; higher layers override per key. Pure so precedence is
/// unit-testable without touching process env.
fn merge_command_tags(
    remote: Option<&std::collections::BTreeMap<String, String>>,
    local: std::collections::HashMap<String, String>,
    env: std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    let mut out: std::collections::HashMap<String, String> = remote
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    out.extend(local); // local overrides remote
    out.extend(env); // env overrides local
    out
}

/// Env-injectable core of [`resolve_slash_command_tags`]: remote → local
/// `[slash_command_tags]` → `env` (highest). Takes the env map explicitly so the
/// TOML-extraction + merge composition is hermetically testable (no process env).
fn resolve_slash_command_tags_with_env(
    effective_config: &TomlValue,
    remote: Option<&std::collections::BTreeMap<String, String>>,
    env: std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    merge_command_tags(remote, slash_command_tags_from_toml(effective_config), env)
}

/// Resolve per-command slash-dropdown tags. Precedence lowest → highest: remote
/// settings (base) → local `[slash_command_tags]` → `ECHO_AGENT_SLASH_COMMAND_TAGS`
/// env var (wins). Empty/missing everywhere → empty map.
pub fn resolve_slash_command_tags(
    effective_config: &TomlValue,
    remote: Option<&std::collections::BTreeMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    resolve_slash_command_tags_with_env(effective_config, remote, slash_command_tags_from_env())
}

/// Read `[cli] channel` from config.toml.
/// Returns `None` when absent (falls through to remote settings).
pub fn channel_from_toml_opt(root: &TomlValue) -> Option<String> {
    if let TomlValue::Table(table) = root
        && let Some(TomlValue::Table(cli)) = table.get("cli")
    {
        cli.get("channel")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::util::config::RemoteSettings;

    use super::*;
    use toml::Value as TomlValue;

    #[test]
    fn show_tips_defaults_to_none() {
        let config = TomlValue::Table(toml::map::Map::new());
        assert_eq!(show_tips_from_toml_opt(&config), None);
    }

    #[test]
    fn show_tips_reads_false() {
        let config: TomlValue = toml::from_str("[cli]\nshow_tips = false").unwrap();
        assert_eq!(show_tips_from_toml_opt(&config), Some(false));
    }

    #[test]
    fn show_tips_reads_true() {
        let config: TomlValue = toml::from_str("[cli]\nshow_tips = true").unwrap();
        assert_eq!(show_tips_from_toml_opt(&config), Some(true));
    }

    #[test]
    fn remote_settings_tips_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.tips, None);
    }

    #[test]
    fn remote_settings_tips_null() {
        let json = r#"{"tips": null}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.tips, None);
    }

    #[test]
    fn remote_settings_tips_empty() {
        let json = r#"{"tips": []}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.tips, Some(vec![]));
    }

    #[test]
    fn remote_settings_tips_populated() {
        let json = r#"{"tips": ["a", "b"]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.tips, Some(vec!["a".to_string(), "b".to_string()]));
    }

    // Hermetic: drive the resolver through `_with_env` with an EXPLICIT env map
    // so ambient `ECHO_AGENT_SLASH_COMMAND_TAGS` can't affect these assertions.
    #[test]
    fn resolve_slash_command_tags_local_overrides_remote_per_key() {
        let mut remote = std::collections::BTreeMap::new();
        remote.insert("workflows".to_string(), "beta".to_string());
        remote.insert("model".to_string(), "remote-only".to_string());
        let local: TomlValue =
            toml::from_str("[slash_command_tags]\nworkflows = \"new\"\nplan = \"local-only\"\n")
                .unwrap();

        let resolved = resolve_slash_command_tags_with_env(
            &local,
            Some(&remote),
            std::collections::HashMap::new(),
        );
        // Local wins per key.
        assert_eq!(resolved.get("workflows").map(String::as_str), Some("new"));
        // Remote-only key passes through.
        assert_eq!(
            resolved.get("model").map(String::as_str),
            Some("remote-only")
        );
        // Local-only key is added.
        assert_eq!(resolved.get("plan").map(String::as_str), Some("local-only"));
        assert_eq!(resolved.len(), 3);
    }

    #[test]
    fn resolve_slash_command_tags_missing_is_empty_and_remote_passes_through() {
        let empty = TomlValue::Table(toml::map::Map::new());
        assert!(
            resolve_slash_command_tags_with_env(&empty, None, std::collections::HashMap::new())
                .is_empty()
        );

        let mut remote = std::collections::BTreeMap::new();
        remote.insert("commit".to_string(), "new".to_string());
        let resolved = resolve_slash_command_tags_with_env(
            &empty,
            Some(&remote),
            std::collections::HashMap::new(),
        );
        assert_eq!(resolved.get("commit").map(String::as_str), Some("new"));
        assert_eq!(resolved.len(), 1);
    }

    // Env wins through the public composition — proven hermetically via `_with_env`
    // (no process-env mutation).
    #[test]
    fn resolve_slash_command_tags_env_overrides_local_and_remote() {
        let mut remote = std::collections::BTreeMap::new();
        remote.insert("workflows".to_string(), "remote".to_string());
        let local: TomlValue =
            toml::from_str("[slash_command_tags]\nworkflows = \"local\"\n").unwrap();
        let mut env = std::collections::HashMap::new();
        env.insert("workflows".to_string(), "env".to_string());

        let resolved = resolve_slash_command_tags_with_env(&local, Some(&remote), env);
        assert_eq!(resolved.get("workflows").map(String::as_str), Some("env"));
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn remote_settings_slash_command_tags_absent_and_malformed() {
        // Absent → None.
        let s: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.slash_command_tags, None);
        // Malformed (array instead of map) → tolerated as None, whole parse ok.
        let s: RemoteSettings =
            serde_json::from_str(r#"{"slash_command_tags": ["oops"]}"#).unwrap();
        assert_eq!(s.slash_command_tags, None);
        // Well-formed map parses.
        let s: RemoteSettings =
            serde_json::from_str(r#"{"slash_command_tags": {"commit": "new"}}"#).unwrap();
        assert_eq!(
            s.slash_command_tags
                .as_ref()
                .and_then(|m| m.get("commit"))
                .map(String::as_str),
            Some("new")
        );
    }

    #[test]
    fn merge_command_tags_env_beats_local_beats_remote_per_key() {
        let mut remote = std::collections::BTreeMap::new();
        remote.insert("a".to_string(), "remote-a".to_string());
        remote.insert("b".to_string(), "remote-b".to_string());
        remote.insert("r".to_string(), "remote-only".to_string());

        let mut local = std::collections::HashMap::new();
        local.insert("a".to_string(), "local-a".to_string());
        local.insert("b".to_string(), "local-b".to_string());
        local.insert("l".to_string(), "local-only".to_string());

        let mut env = std::collections::HashMap::new();
        env.insert("a".to_string(), "env-a".to_string());
        env.insert("e".to_string(), "env-only".to_string());

        let merged = merge_command_tags(Some(&remote), local, env);
        assert_eq!(merged.get("a").map(String::as_str), Some("env-a")); // env > local > remote
        assert_eq!(merged.get("b").map(String::as_str), Some("local-b")); // local > remote (no env)
        assert_eq!(merged.get("r").map(String::as_str), Some("remote-only")); // remote-only survives
        assert_eq!(merged.get("l").map(String::as_str), Some("local-only")); // local-only survives
        assert_eq!(merged.get("e").map(String::as_str), Some("env-only")); // env-only survives
        assert_eq!(merged.len(), 5);

        // All sources empty → empty map.
        assert!(
            merge_command_tags(
                None,
                std::collections::HashMap::new(),
                std::collections::HashMap::new()
            )
            .is_empty()
        );
    }

    #[test]
    fn parse_slash_command_tags_json_handles_none_valid_and_malformed() {
        // Unset → empty (no warn).
        assert!(parse_slash_command_tags_json(None).is_empty());
        // Empty / whitespace-only is the normal "no override" state → empty (no warn).
        assert!(parse_slash_command_tags_json(Some("")).is_empty());
        assert!(parse_slash_command_tags_json(Some("   ")).is_empty());
        // Valid JSON object of string→string → parsed.
        let parsed = parse_slash_command_tags_json(Some(r#"{"commit":"new","plan":"beta"}"#));
        assert_eq!(parsed.get("commit").map(String::as_str), Some("new"));
        assert_eq!(parsed.get("plan").map(String::as_str), Some("beta"));
        assert_eq!(parsed.len(), 2);
        // Array instead of object → empty (tolerated).
        assert!(parse_slash_command_tags_json(Some(r#"["oops"]"#)).is_empty());
        // Non-string value → whole parse fails → empty (only string values kept).
        assert!(parse_slash_command_tags_json(Some(r#"{"commit": 3}"#)).is_empty());
        // Not JSON → empty.
        assert!(parse_slash_command_tags_json(Some("garbage")).is_empty());
    }
}
