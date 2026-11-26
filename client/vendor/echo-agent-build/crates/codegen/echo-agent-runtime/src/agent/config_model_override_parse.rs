//! Resilient parsing for `[model.<id>]` TOML overrides.
//!
//! It also defines [`ConfigWarning`] and [`WarningTarget`], the shared warning
//! vocabulary; the `[auth_provider.*]` parser in `config.rs` emits them too.
//!
//! A model entry must survive a bad field: warn and skip the field, never
//! drop the model (managed configs must not lose catalog entries).
//!
//! Every table is deserialized through `serde_ignored`, so unknown fields
//! warn on every path and [`ConfigModelOverride`] stays the single source of
//! truth for the field set. When the whole-table parse fails, fields that
//! fail to parse on their own are pruned (one warning each) and the table is
//! parsed again. Non-table values are dropped with a warning.
//!
//! Warnings are retained on `Config::config_warnings` and surfaced by
//! `echoagent inspect`.

use indexmap::IndexMap;
use serde::Serialize;

use super::config::ConfigModelOverride;

/// Category for a [`ConfigWarning`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigWarningKind {
    /// Field name not recognized; field ignored.
    UnknownField,
    /// Value failed to parse; field skipped.
    InvalidValue,
    /// Legacy alias given alongside its canonical key; alias skipped.
    DuplicateAlias,
    /// Entry value is not a TOML table; entry dropped.
    NotATable,
    /// Fields are individually valid but conflict (e.g. `auth_provider`
    /// shadowed by `api_key`/`env_key`); all fields kept, one is inert.
    ConflictingFields,
    /// Entry failed to parse even after skipping invalid fields; the model
    /// keeps an empty override.
    UnparseableEntry,
}

/// What a [`ConfigWarning`] is about. Serialize-only: `echoagent inspect --json`
/// emits it, nothing deserializes it back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "target", rename_all = "camelCase")]
pub enum WarningTarget {
    /// The `[model]` section as a whole (e.g. not a table).
    ModelSection,
    /// A `[model.<key>]` entry; `field` names a key when the warning is
    /// field-specific.
    Model {
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    /// The `[auth_provider]` section as a whole.
    AuthProviderSection,
    /// An `[auth_provider.<name>]` table; `field` names a key when the
    /// warning is field-specific.
    AuthProvider {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    ModelProviderSection,
    ModelProvider {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    ConfigKey {
        path: String,
    },
}

impl WarningTarget {
    /// The config path, e.g. `model."echo-agent-4.5"` or `auth_provider."litellm"`.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::ModelSection => "model".to_owned(),
            Self::Model { key, .. } => format!("model.\"{key}\""),
            Self::AuthProviderSection => "auth_provider".to_owned(),
            Self::AuthProvider { name, .. } => format!("auth_provider.\"{name}\""),
            Self::ModelProviderSection => "model_providers".to_owned(),
            Self::ModelProvider { id, .. } => format!("model_providers.\"{id}\""),
            Self::ConfigKey { path } => path.clone(),
        }
    }

    pub(crate) fn field(&self) -> Option<&str> {
        match self {
            Self::Model { field, .. }
            | Self::AuthProvider { field, .. }
            | Self::ModelProvider { field, .. } => field.as_deref(),
            Self::ModelSection
            | Self::AuthProviderSection
            | Self::ModelProviderSection
            | Self::ConfigKey { .. } => None,
        }
    }
}

/// One skipped field or dropped entry from config parsing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWarning {
    #[serde(flatten)]
    pub target: WarningTarget,
    pub kind: ConfigWarningKind,
    pub reason: String,
}

impl ConfigWarning {
    pub(crate) fn model(
        key: &str,
        field: Option<&str>,
        kind: ConfigWarningKind,
        reason: String,
    ) -> Self {
        let target = WarningTarget::Model {
            key: key.to_owned(),
            field: field.map(str::to_owned),
        };
        Self {
            target,
            kind,
            reason,
        }
    }

    pub(crate) fn model_section(kind: ConfigWarningKind, reason: String) -> Self {
        Self {
            target: WarningTarget::ModelSection,
            kind,
            reason,
        }
    }

    pub(crate) fn auth_provider(
        name: &str,
        field: Option<&str>,
        kind: ConfigWarningKind,
        reason: String,
    ) -> Self {
        let target = WarningTarget::AuthProvider {
            name: name.to_owned(),
            field: field.map(str::to_owned),
        };
        Self {
            target,
            kind,
            reason,
        }
    }

    pub(crate) fn auth_provider_section(kind: ConfigWarningKind, reason: String) -> Self {
        Self {
            target: WarningTarget::AuthProviderSection,
            kind,
            reason,
        }
    }

    pub(crate) fn model_provider(
        id: &str,
        field: Option<&str>,
        kind: ConfigWarningKind,
        reason: String,
    ) -> Self {
        Self {
            target: WarningTarget::ModelProvider {
                id: id.to_owned(),
                field: field.map(str::to_owned),
            },
            kind,
            reason,
        }
    }

    pub(crate) fn model_provider_section(kind: ConfigWarningKind, reason: String) -> Self {
        Self {
            target: WarningTarget::ModelProviderSection,
            kind,
            reason,
        }
    }

    pub(crate) fn config_key(path: String, kind: ConfigWarningKind, reason: String) -> Self {
        Self {
            target: WarningTarget::ConfigKey { path },
            kind,
            reason,
        }
    }

    pub(crate) fn field(&self) -> Option<&str> {
        self.target.field()
    }
}

pub(crate) struct ParsedModelOverrides {
    pub models: IndexMap<String, ConfigModelOverride>,
    pub warnings: Vec<ConfigWarning>,
}

/// Parses every `[model.<id>]` entry in `raw_config`, returning the overrides
/// and a warning for each skipped field or dropped entry.
pub(crate) fn parse_model_overrides(raw_config: &toml::Value) -> ParsedModelOverrides {
    let mut models = IndexMap::new();
    let mut warnings = Vec::new();
    let Some(section) = raw_config.get("model") else {
        return ParsedModelOverrides { models, warnings };
    };
    let Some(table) = section.as_table() else {
        warnings.push(ConfigWarning::model_section(
            ConfigWarningKind::NotATable,
            format!(
                "`model` must be a table of [model.<id>] entries, got {}; all model overrides ignored",
                section.type_str()
            ),
        ));
        return ParsedModelOverrides { models, warnings };
    };
    for (model_key, value) in table {
        let Some(entry_table) = value.as_table() else {
            warnings.push(ConfigWarning::model(
                model_key,
                None,
                ConfigWarningKind::NotATable,
                format!(
                    "expected a table like [model.\"{model_key}\"], got {}; entry dropped",
                    value.type_str()
                ),
            ));
            continue;
        };
        let (entry, entry_warnings) = parse_model_override_table(model_key, entry_table.clone());
        warnings.extend(entry_warnings);
        models.insert(model_key.clone(), entry);
    }
    ParsedModelOverrides { models, warnings }
}

/// Logs the warnings when they differ from the previous parse, so a
/// persistently broken config logs once per process instead of once per parse.
pub(crate) fn log_config_warnings(warnings: &[ConfigWarning]) {
    use std::hash::{Hash as _, Hasher as _};
    use std::sync::atomic::{AtomicU64, Ordering};

    static LAST_LOGGED: AtomicU64 = AtomicU64::new(0);
    // 0 means "no warnings"; real hashes are clamped to nonzero.
    let hash = if warnings.is_empty() {
        0
    } else {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        warnings.hash(&mut hasher);
        hasher.finish().max(1)
    };
    if LAST_LOGGED.swap(hash, Ordering::Relaxed) == hash {
        return;
    }

    for warning in warnings {
        tracing::warn!(
            path = %warning.target.label(),
            field = warning.field().unwrap_or("(entry)"),
            kind = ?warning.kind,
            reason = %warning.reason,
            "config: ignored unrecognized or invalid entry"
        );
    }
    if !warnings.is_empty() {
        tracing::warn!(
            warnings = warnings.len(),
            "config: parsed with warnings; run `echoagent inspect` for details"
        );
    }
}

fn parse_model_override_table(
    model_key: &str,
    mut table: toml::map::Map<String, toml::Value>,
) -> (ConfigModelOverride, Vec<ConfigWarning>) {
    let mut warnings = Vec::new();
    dedupe_aliases(model_key, &mut table, &mut warnings);

    // Unknown-field warnings come from whichever parse produces the returned
    // entry, so both paths report them identically.
    let (entry, mut warnings) = match deserialize_with_unknown_fields(table.clone()) {
        Ok((entry, unknown)) => {
            warnings.extend(unknown_field_warnings(model_key, unknown));
            (entry, warnings)
        }
        Err(_) => {
            prune_invalid_fields(model_key, &mut table, &mut warnings);
            match deserialize_with_unknown_fields(table) {
                Ok((entry, unknown)) => {
                    warnings.extend(unknown_field_warnings(model_key, unknown));
                    (entry, warnings)
                }
                Err(error) => {
                    // Reachable only when fields conflict jointly, e.g. an
                    // alias pair missing from `ALIASES`. Keep the model
                    // rather than dropping it.
                    warnings.push(ConfigWarning::model(
                        model_key,
                        None,
                        ConfigWarningKind::UnparseableEntry,
                        format!(
                            "failed to parse after skipping invalid fields ({error}); using empty override"
                        ),
                    ));
                    (ConfigModelOverride::default(), warnings)
                }
            }
        }
    };

    if entry.auth_provider.is_some() {
        // A non-empty `api_key` always shadows; an `env_key` only shadows when
        // its variable resolves at runtime, which parse time can't know. Warn
        // accordingly so the message matches what actually happens.
        let has_static_api_key = entry
            .api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|k| !k.is_empty());
        if has_static_api_key {
            warnings.push(ConfigWarning::model(
                model_key,
                Some("auth_provider"),
                ConfigWarningKind::ConflictingFields,
                "auth_provider is shadowed by api_key on this model; the static \
                 key always takes precedence, so the provider never runs"
                    .to_owned(),
            ));
        } else if entry
            .env_key
            .as_ref()
            .and_then(crate::agent::config::EnvKeys::primary)
            .is_some()
        {
            warnings.push(ConfigWarning::model(
                model_key,
                Some("auth_provider"),
                ConfigWarningKind::ConflictingFields,
                "auth_provider may be shadowed by env_key on this model; env_key \
                 takes precedence when its variable resolves to a value, \
                 otherwise the provider runs"
                    .to_owned(),
            ));
        }
    }

    (entry, warnings)
}

/// `(canonical, legacy)` key pairs that serde rejects as duplicate fields
/// when both appear in one table. Keep in sync with the `#[serde(alias)]`
/// attributes on [`ConfigModelOverride`].
const ALIASES: &[(&str, &str)] = &[("compactions_remaining", "send_compactions_remaining")];

/// Removes one key of each [`ALIASES`] pair that appears twice in `table`.
/// The canonical key wins; when its value doesn't parse, the legacy key is
/// kept instead.
fn dedupe_aliases(
    model_key: &str,
    table: &mut toml::map::Map<String, toml::Value>,
    warnings: &mut Vec<ConfigWarning>,
) {
    for &(canonical, legacy) in ALIASES {
        if !(table.contains_key(canonical) && table.contains_key(legacy)) {
            continue;
        }
        match field_parse_error(canonical, &table[canonical]) {
            None => {
                table.remove(legacy);
                warnings.push(ConfigWarning::model(
                    model_key,
                    Some(legacy),
                    ConfigWarningKind::DuplicateAlias,
                    format!("legacy alias of {canonical}; skipped in favor of {canonical}"),
                ));
            }
            Some(error) => {
                table.remove(canonical);
                warnings.push(ConfigWarning::model(
                    model_key,
                    Some(canonical),
                    ConfigWarningKind::InvalidValue,
                    format!("{error}; skipped in favor of {legacy}"),
                ));
            }
        }
    }
}

/// Deserializes `table`, also returning the unknown field names that serde
/// would otherwise silently discard.
fn deserialize_with_unknown_fields(
    table: toml::map::Map<String, toml::Value>,
) -> Result<(ConfigModelOverride, Vec<String>), toml::de::Error> {
    let mut unknown = Vec::new();
    let entry = serde_ignored::deserialize(toml::Value::Table(table), |path| {
        unknown.push(path.to_string());
    })?;
    Ok((entry, unknown))
}

fn unknown_field_warnings(model_key: &str, unknown: Vec<String>) -> Vec<ConfigWarning> {
    unknown
        .into_iter()
        .map(|field| {
            ConfigWarning::model(
                model_key,
                Some(field.as_str()),
                ConfigWarningKind::UnknownField,
                "unknown field".to_owned(),
            )
        })
        .collect()
}

/// Removes each field that fails to parse on its own, one warning per field.
/// Unknown fields stay; the follow-up parse reports them.
fn prune_invalid_fields(
    model_key: &str,
    table: &mut toml::map::Map<String, toml::Value>,
    warnings: &mut Vec<ConfigWarning>,
) {
    table.retain(|field, value| match field_parse_error(field, value) {
        None => true,
        Some(error) => {
            warnings.push(ConfigWarning::model(
                model_key,
                Some(field),
                ConfigWarningKind::InvalidValue,
                error.to_string(),
            ));
            false
        }
    });
}

/// Parses `field` in isolation, returning the error if it fails.
fn field_parse_error(field: &str, value: &toml::Value) -> Option<toml::de::Error> {
    let mut singleton = toml::map::Map::new();
    singleton.insert(field.to_owned(), value.clone());
    toml::Value::Table(singleton)
        .try_into::<ConfigModelOverride>()
        .err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampling::ApiBackend;
    use echo_agent_sampling_types::{
        CompactionAtTokens, CompactionsRemaining, ReasoningEffort, ReasoningEffortOption,
    };

    fn parse_cfg(toml_str: &str) -> crate::agent::config::Config {
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        crate::agent::config::Config::new_from_toml_cfg(&raw).expect("config should parse")
    }

    fn parse_raw(toml_str: &str) -> (IndexMap<String, ConfigModelOverride>, Vec<ConfigWarning>) {
        let raw: toml::Value = toml::from_str(toml_str).unwrap();
        let ParsedModelOverrides { models, warnings } = parse_model_overrides(&raw);
        (models, warnings)
    }

    #[test]
    fn duplicate_compactions_keys_keeps_model() {
        let cfg = parse_cfg(
            r#"
            [model."echo-agent-4.5"]
            model = "echo-agent-4.5"
            env_key = "ANTHROPIC_AUTH_TOKEN"
            compactions_remaining = 1
            send_compactions_remaining = true
            "#,
        );
        let model = cfg
            .config_models
            .get("echo-agent-4.5")
            .expect("echo-agent-4.5 must remain in catalog");
        assert_eq!(
            model.compactions_remaining,
            Some(CompactionsRemaining::Fixed(1))
        );
        assert!(cfg.config_warnings.iter().any(|w| {
            w.kind == ConfigWarningKind::DuplicateAlias
                && w.field() == Some("send_compactions_remaining")
        }));
        let resolved = crate::agent::config::resolve_model_list(&cfg, None);
        assert!(resolved.contains_key("echo-agent-4.5"));
    }

    #[test]
    fn legacy_alias_alone_parses_without_warning() {
        let cfg = parse_cfg(
            r#"
            [model."echo-agent-4.5"]
            model = "echo-agent-4.5"
            send_compactions_remaining = 2
            "#,
        );
        let model = cfg.config_models.get("echo-agent-4.5").unwrap();
        assert_eq!(
            model.compactions_remaining,
            Some(CompactionsRemaining::Fixed(2))
        );
        assert!(cfg.config_warnings.is_empty());
    }

    #[test]
    fn invalid_reasoning_effort_skips_field_keeps_model() {
        let cfg = parse_cfg(
            r#"
            [model."echo-agent-4.5"]
            model = "echo-agent-4.5"
            env_key = "ANTHROPIC_AUTH_TOKEN"
            reasoning_effort = "not-a-level"
            "#,
        );
        let model = cfg
            .config_models
            .get("echo-agent-4.5")
            .expect("echo-agent-4.5 must remain in catalog");
        assert_eq!(model.model.as_deref(), Some("echo-agent-4.5"));
        assert!(model.reasoning_effort.is_none());
        assert!(cfg.config_warnings.iter().any(|w| {
            w.kind == ConfigWarningKind::InvalidValue && w.field() == Some("reasoning_effort")
        }));
    }

    #[test]
    fn unknown_field_warns_but_keeps_known_fields() {
        let (models, warnings) = parse_raw(
            r#"
            [model."echo-agent-4.5"]
            model = "echo-agent-4.5"
            env_key = "TOKEN"
            future_field = 1
            "#,
        );
        let entry = models.get("echo-agent-4.5").unwrap();
        assert_eq!(entry.model.as_deref(), Some("echo-agent-4.5"));
        assert_eq!(
            entry.env_key.as_ref().and_then(|k| k.primary()),
            Some("TOKEN")
        );
        assert_eq!(
            warnings,
            vec![ConfigWarning::model(
                "echo-agent-4.5",
                Some("future_field"),
                ConfigWarningKind::UnknownField,
                "unknown field".to_owned(),
            )]
        );
    }

    /// An unknown field warns the same whether or not another field fails to
    /// parse.
    #[test]
    fn unknown_field_warning_is_path_independent() {
        let unknown_of = |toml_str: &str| {
            let (_, warnings) = parse_raw(toml_str);
            warnings
                .into_iter()
                .filter(|w| w.kind == ConfigWarningKind::UnknownField)
                .collect::<Vec<_>>()
        };
        let fast = unknown_of(
            r#"
            [model.m]
            temprature = 0.5
            "#,
        );
        let slow = unknown_of(
            r#"
            [model.m]
            temprature = 0.5
            reasoning_effort = "not-a-level"
            "#,
        );
        assert_eq!(fast, slow);
        assert_eq!(fast.len(), 1);
        assert_eq!(fast[0].field(), Some("temprature"));
    }

    #[test]
    fn prune_skips_invalid_fields_and_keeps_the_rest() {
        // A valid nested table survives an invalid sibling.
        let (models, warnings) = parse_raw(
            r#"
            [model.m]
            temperature = "hot"
            [model.m.extra_headers]
            x-team = "codegen"
            "#,
        );
        let entry = models.get("m").unwrap();
        assert_eq!(
            entry.extra_headers.get("x-team").map(String::as_str),
            Some("codegen")
        );
        assert!(entry.temperature.is_none());
        assert!(warnings.iter().any(|w| {
            w.kind == ConfigWarningKind::InvalidValue && w.field() == Some("temperature")
        }));

        // All fields invalid: the model stays, with an empty override.
        let (models, warnings) = parse_raw(
            r#"
            [model.m]
            temperature = "hot"
            max_retries = "many"
            "#,
        );
        let entry = models.get("m").expect("model must remain in catalog");
        assert!(entry.temperature.is_none());
        assert!(entry.max_retries.is_none());
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .all(|w| w.kind == ConfigWarningKind::InvalidValue)
        );
    }

    #[test]
    fn invalid_canonical_key_falls_back_to_legacy_alias() {
        let (models, warnings) = parse_raw(
            r#"
            [model.m]
            compactions_remaining = "bad"
            send_compactions_remaining = 2
            "#,
        );
        let entry = models.get("m").unwrap();
        assert_eq!(
            entry.compactions_remaining,
            Some(CompactionsRemaining::Fixed(2))
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ConfigWarningKind::InvalidValue);
        assert_eq!(warnings[0].field(), Some("compactions_remaining"));
    }

    #[test]
    fn non_table_model_section_warns_and_is_ignored() {
        let (models, warnings) = parse_raw(r#"model = "echo-agent-4""#);
        assert!(models.is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ConfigWarningKind::NotATable);
        assert!(matches!(warnings[0].target, WarningTarget::ModelSection));
    }

    #[test]
    fn non_table_entry_is_dropped_with_warning() {
        let (models, warnings) = parse_raw(
            r#"
            [model]
            oops = 5
            "#,
        );
        assert!(models.is_empty(), "a scalar cannot define a model");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ConfigWarningKind::NotATable);
        assert!(matches!(
            &warnings[0].target,
            WarningTarget::Model { key, field: None } if key == "oops"
        ));
    }

    /// Exhaustive literal (no `..`): a new struct field is a compile error
    /// here until the drift-guard tests cover it.
    fn fully_populated_override() -> ConfigModelOverride {
        ConfigModelOverride {
            model: Some("m".into()),
            model_family: None,
            base_url: Some("https://example.com".into()),
            name: Some("Model M".into()),
            description: Some("desc".into()),
            api_key: Some("key".into()),
            env_key: Some(crate::agent::config::EnvKeys::single("ENV_KEY")),
            auth_provider: Some("corp-gateway".into()),
            model_provider: Some("gateway".into()),
            api_base_url: Some("https://api.example.com".into()),
            max_completion_tokens: Some(1024),
            temperature: Some(0.5),
            top_p: Some(0.9),
            api_backend: Some(ApiBackend::Messages),
            auth_scheme: Some(echo_agent_sampler::AuthScheme::XApiKey),
            extra_headers: [("x-team".to_owned(), "codegen".to_owned())]
                .into_iter()
                .collect(),
            query_params: [("api-version".to_owned(), "2026-07-22".to_owned())]
                .into_iter()
                .collect(),
            env_http_headers: [("x-tenant-token".to_owned(), "TENANT_TOKEN_VAR".to_owned())]
                .into_iter()
                .collect(),
            context_window: Some(200_000),
            auto_compact_threshold_percent: Some(80),
            system_prompt_label: Some("label".into()),
            use_concise: Some(true),
            agent_type: Some("agent".into()),
            inference_idle_timeout_secs: Some(60),
            max_retries: Some(3),
            subagent_rate_limit_max_attempts: Some(8),
            hidden: Some(false),
            supported_in_api: Some(true),
            reasoning_effort: Some(ReasoningEffort::High),
            supports_reasoning_effort: Some(true),
            reasoning_efforts: Some(vec![ReasoningEffortOption {
                id: "deep".to_string(),
                value: ReasoningEffort::High,
                label: "Deep".to_string(),
                description: Some("Deep reasoning".to_string()),
                default: true,
            }]),
            supports_backend_search: Some(false),
            compactions_remaining: Some(CompactionsRemaining::Fixed(1)),
            compaction_at_tokens: Some(CompactionAtTokens::Fixed(100_000)),
            show_model_fingerprint: Some(true),
            stream_tool_calls: Some(false),
            supports_tool_calls: None,
            supports_images: None,
            reasoning_effort_auto: None,
        }
    }

    fn parse_single_entry(
        entry: toml::map::Map<String, toml::Value>,
    ) -> (IndexMap<String, ConfigModelOverride>, Vec<ConfigWarning>) {
        let mut model_table = toml::map::Map::new();
        model_table.insert("m".to_owned(), toml::Value::Table(entry));
        let mut root = toml::map::Map::new();
        root.insert("model".to_owned(), toml::Value::Table(model_table));
        let ParsedModelOverrides { models, warnings } =
            parse_model_overrides(&toml::Value::Table(root));
        (models, warnings)
    }

    #[test]
    fn fully_populated_override_round_trips_with_only_the_shadowing_warning() {
        let serialized = toml::Value::try_from(fully_populated_override()).unwrap();
        let (models, warnings) = parse_single_entry(serialized.as_table().unwrap().clone());
        // The exhaustive literal deliberately sets `api_key`, `env_key`, AND
        // `auth_provider`: the one legal-but-warned combination. Any other
        // warning (skipped/unknown field) still fails the guard.
        let unexpected: Vec<_> = warnings
            .iter()
            .filter(|w| w.kind != ConfigWarningKind::ConflictingFields)
            .collect();
        assert_eq!(unexpected, Vec::<&ConfigWarning>::new());
        assert_eq!(warnings.len(), 1);
        let reparsed = toml::Value::try_from(models.get("m").unwrap()).unwrap();
        assert_eq!(reparsed, serialized, "round-trip must be lossless");
    }

    /// `auth_provider` alongside `api_key`/`env_key` warns (static keys
    /// win in `resolve_credentials`, so the provider never runs) but keeps
    /// both fields.
    #[test]
    fn auth_provider_shadowed_by_static_key_warns() {
        let mut entry = toml::map::Map::new();
        entry.insert("api_key".to_owned(), toml::Value::String("sk-x".into()));
        entry.insert(
            "auth_provider".to_owned(),
            toml::Value::String("corp".into()),
        );
        let (models, warnings) = parse_single_entry(entry);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ConfigWarningKind::ConflictingFields);
        assert_eq!(warnings[0].field(), Some("auth_provider"));
        let parsed = models.get("m").unwrap();
        assert_eq!(parsed.api_key.as_deref(), Some("sk-x"));
        assert_eq!(parsed.auth_provider.as_deref(), Some("corp"));

        // Provider alone: no warning.
        let mut entry = toml::map::Map::new();
        entry.insert(
            "auth_provider".to_owned(),
            toml::Value::String("corp".into()),
        );
        let (_, warnings) = parse_single_entry(entry);
        assert_eq!(warnings, Vec::new());

        // env_key is only a conditional shadow: warn, but as "may be shadowed".
        let mut entry = toml::map::Map::new();
        entry.insert("env_key".to_owned(), toml::Value::String("MY_KEY".into()));
        entry.insert(
            "auth_provider".to_owned(),
            toml::Value::String("corp".into()),
        );
        let (_, warnings) = parse_single_entry(entry);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, ConfigWarningKind::ConflictingFields);
        assert!(warnings[0].reason.contains("may be shadowed"));

        // An empty api_key does not shadow, so it must not warn.
        let mut entry = toml::map::Map::new();
        entry.insert("api_key".to_owned(), toml::Value::String("  ".into()));
        entry.insert(
            "auth_provider".to_owned(),
            toml::Value::String("corp".into()),
        );
        let (_, warnings) = parse_single_entry(entry);
        assert_eq!(warnings, Vec::new());
    }

    /// Drift guard: every `#[serde(alias)]` on [`ConfigModelOverride`] must
    /// have a matching `ALIASES` pair, and vice versa. An unregistered alias
    /// would send both-keys configs to the empty-override fallback.
    #[test]
    fn every_struct_alias_is_registered_in_aliases() {
        let source = include_str!("config.rs");
        let start = source
            .find("pub struct ConfigModelOverride {")
            .expect("ConfigModelOverride definition in config.rs");
        let block = &source[start..];
        let block = &block[..block.find("\n}").expect("struct end")];

        let mut found = Vec::new();
        let mut rest = block;
        while let Some(pos) = rest.find("#[serde(alias = \"") {
            let after = &rest[pos + "#[serde(alias = \"".len()..];
            let legacy = &after[..after.find('"').expect("closing quote")];
            let field = &after[after.find("pub ").expect("field after alias") + 4..];
            let canonical = &field[..field.find(':').expect("field type colon")];
            found.push((canonical.to_owned(), legacy.to_owned()));
            rest = after;
        }
        assert_eq!(
            block.matches("alias").count(),
            found.len(),
            "an alias on ConfigModelOverride was not recognized; write it as \
             `#[serde(alias = \"...\")]` on its own line, or update this scan"
        );
        found.sort();

        let mut registered: Vec<(String, String)> = ALIASES
            .iter()
            .map(|&(c, l)| (c.to_owned(), l.to_owned()))
            .collect();
        registered.sort();
        assert_eq!(
            found, registered,
            "#[serde(alias)] attributes on ConfigModelOverride and ALIASES must match"
        );
    }

    /// Drift guard for `ALIASES`, in both directions: every pair must be a
    /// real serde alias (a both-keys table fails a plain parse), and the
    /// parser must resolve it to the canonical key with a single warning.
    #[test]
    fn every_aliases_pair_is_a_real_serde_alias_and_dedupes() {
        let reference = toml::Value::try_from(fully_populated_override()).unwrap();
        for &(canonical, legacy) in ALIASES {
            let value = reference
                .get(canonical)
                .unwrap_or_else(|| panic!("{canonical} missing from fully_populated_override"));
            let mut entry = toml::map::Map::new();
            entry.insert(canonical.to_owned(), value.clone());
            entry.insert(legacy.to_owned(), value.clone());
            assert!(
                toml::Value::Table(entry.clone())
                    .try_into::<ConfigModelOverride>()
                    .is_err(),
                "{canonical}/{legacy} is not a serde alias pair; remove it from ALIASES"
            );

            let (models, warnings) = parse_single_entry(entry);
            let parsed = toml::Value::try_from(models.get("m").unwrap()).unwrap();
            assert_eq!(
                parsed.get(canonical),
                Some(value),
                "canonical value must be retained"
            );
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].kind, ConfigWarningKind::DuplicateAlias);
            assert_eq!(warnings[0].field(), Some(legacy));
        }
    }
}
