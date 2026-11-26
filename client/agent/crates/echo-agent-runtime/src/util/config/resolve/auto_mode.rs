use crate::util::config::RemoteSettings;
use toml::Value as TomlValue;

/// Env override for the **auto** permission-mode feature gate.
pub(crate) const ENV_AUTO_PERMISSION_MODE: &str = "ECHO_AGENT_AUTO_PERMISSION_MODE";

const AUTO_MODE_CLASSIFY_TIMEOUT_MIN_MS: u64 = 1_000;
const AUTO_MODE_CLASSIFY_TIMEOUT_DEFAULT_MS: u64 = 30_000;
const AUTO_MODE_CLASSIFY_TIMEOUT_MAX_MS: u64 = 120_000;

/// Crate-wide serialization lock for tests that mutate
/// `ECHO_AGENT_AUTO_PERMISSION_MODE`. Every test reading the gate (here and in
/// `permissions.rs`, compiled into the same test binary) locks this so a
/// concurrent setter can't make them flaky.
#[cfg(test)]
pub(crate) static AUTO_PERMISSION_MODE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Extract the `[auto_mode] enabled` gate from one TOML layer (the local opt-in
/// that replaced `[features] auto_permission_mode`).
fn auto_permission_mode_from_toml(v: Option<&TomlValue>) -> Option<bool> {
    v?.get("auto_mode")?.get("enabled")?.as_bool()
}

/// Coerce a present raw remote settings `auto_mode` JSON value into the shell's typed
/// [`AutoModeConfig`]. Coercion is all-or-nothing: any malformed field (e.g. a
/// bad `prompt_type` enum value) drops the WHOLE object to `None` (falls through
/// to the gate default). A present-but-malformed payload is warned.
fn coerce_auto_mode_json(value: serde_json::Value) -> Option<crate::agent::config::AutoModeConfig> {
    match serde_json::from_value(value) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            tracing::warn!(error = %e, "[auto_mode]: dropped malformed remote payload");
            None
        }
    }
}

/// Coerce the raw remote settings `auto_mode` JSON on `RemoteSettings` (absent ⇒
/// `None`, silently; present-but-malformed ⇒ `None`, warned).
fn coerce_remote_auto_mode(
    remote: Option<&RemoteSettings>,
) -> Option<crate::agent::config::AutoModeConfig> {
    coerce_auto_mode_json(remote?.auto_mode.clone()?)
}

/// Coerce a `RemoteSettings`' raw `auto_mode` JSON down to just the gate
/// `enabled` bool, for the shell→pager `SettingsUpdateNotification` (the pager
/// only needs the kill-switch, not the full config).
pub(crate) fn remote_auto_mode_enabled(remote: Option<&RemoteSettings>) -> Option<bool> {
    coerce_remote_auto_mode(remote).and_then(|c| c.enabled)
}

/// Pure precedence core for the auto-permission-mode gate, shared by the
/// `RemoteSettings`-typed resolver and the free-function disk reader so the
/// two can't drift. Precedence: requirement > env (`ECHO_AGENT_AUTO_PERMISSION_MODE`)
/// > config > managed > remote feature-flag > default (`true`).
fn resolve_auto_permission_mode_layers(
    requirement: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> crate::agent::config::Resolved<bool> {
    use crate::agent::config::BoolFlag;
    BoolFlag::env(ENV_AUTO_PERMISSION_MODE)
        .requirement(requirement)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}

/// Resolve whether the **auto** permission mode feature (`PermissionMode::Auto`,
/// the LLM/heuristic classifier) is enabled. Full chain mirroring
/// [`resolve_zdr_access_enabled`](super::resolve_zdr_access_enabled):
///
/// requirements > env (`ECHO_AGENT_AUTO_PERMISSION_MODE`) > `[auto_mode] enabled` in
/// `config.toml` > managed > remote settings (`auto_mode.enabled`, coerced
/// from the raw JSON) > default (`true`).
///
/// Default ON: Auto is offered unless a higher layer pins it off. Returns
/// [`Resolved`] so callers can log the winning source.
pub fn resolve_auto_permission_mode_enabled(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> crate::agent::config::Resolved<bool> {
    resolve_auto_permission_mode_layers(
        auto_permission_mode_from_toml(requirements),
        auto_permission_mode_from_toml(user),
        auto_permission_mode_from_toml(managed),
        coerce_remote_auto_mode(remote).and_then(|c| c.enabled),
    )
}

/// Single source of truth for the remote settings `auto_mode` config at free-function
/// call sites that don't hold a live `RemoteSettings` (gate launch decision,
/// pager kill-switch, classifier wiring). Coerced once on cache; the gate reads
/// `.enabled` off it. Lock poisoning is treated fail-safe (`.read().ok()` etc.).
static REMOTE_AUTO_MODE_CONFIG: std::sync::RwLock<Option<crate::agent::config::AutoModeConfig>> =
    std::sync::RwLock::new(None);

/// Record the full remote settings `auto_mode` JSON (coerced once) for the
/// free-function resolvers. Call wherever `RemoteSettings` is applied.
pub fn cache_remote_auto_mode(value: Option<serde_json::Value>) {
    let coerced = value.and_then(coerce_auto_mode_json);
    if let Ok(mut guard) = REMOTE_AUTO_MODE_CONFIG.write() {
        *guard = coerced;
    }
}

/// Update ONLY the gate `enabled` in the cached remote config (the pager
/// kill-switch path carries just the bool). Seeds a default config first so
/// `prompt_type`/`classifier_model`/`reasoning_effort` are not clobbered.
pub fn cache_remote_auto_permission_mode_enabled(value: Option<bool>) {
    if let Ok(mut guard) = REMOTE_AUTO_MODE_CONFIG.write() {
        guard.get_or_insert_with(Default::default).enabled = value;
    }
}

fn cached_remote_auto_permission_mode_enabled() -> Option<bool> {
    REMOTE_AUTO_MODE_CONFIG
        .read()
        .ok()
        .and_then(|g| g.as_ref().and_then(|c| c.enabled))
}

/// Deserialize the `[auto_mode]` table from one effective-config TOML layer into
/// the typed [`AutoModeConfig`]. A malformed table is dropped to `None` (warned,
/// not silently swallowed, so a bad local `[auto_mode]` is visible in logs).
fn auto_mode_config_from_toml(
    v: Option<&TomlValue>,
) -> Option<crate::agent::config::AutoModeConfig> {
    let table = v?.get("auto_mode")?.clone();
    table
        .try_into()
        .map_err(|e| tracing::warn!(error = %e, "[auto_mode]: dropped malformed local table"))
        .ok()
}

fn auto_permission_mode_enabled_from_layers(
    layers: &crate::config::ConfigLayers,
    requirements: Option<&TomlValue>,
    remote: Option<bool>,
) -> bool {
    let crate::config::ConfigLayers {
        system_managed,
        managed,
        user,
        env_overlay: _,
        user_requirements: _,
        system_requirements: _,
        mdm_requirements: _,
        campaigns: _,
    } = layers;
    resolve_auto_permission_mode_layers(
        auto_permission_mode_from_toml(requirements),
        auto_permission_mode_from_toml(Some(user)),
        auto_permission_mode_from_toml(Some(managed))
            .or_else(|| auto_permission_mode_from_toml(Some(system_managed))),
        remote,
    )
    .value
}

fn auto_mode_config_overlay_free(
    layers: &crate::config::ConfigLayers,
) -> crate::agent::config::AutoModeConfig {
    auto_mode_config_from_toml(Some(&layers.effective_config_base_without_overlay()))
        .unwrap_or_default()
}

/// Disk form of the gate (overlay-free). Defaults `true`.
pub fn auto_permission_mode_enabled_from_disk() -> bool {
    let requirements = crate::config::load_merged_requirements();
    let layers = match crate::config::ConfigLayers::load() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "auto_permission_mode: failed to load config layers");
            crate::config::ConfigLayers::default()
        }
    };
    auto_permission_mode_enabled_from_layers(
        &layers,
        requirements.as_ref(),
        cached_remote_auto_permission_mode_enabled(),
    )
}

/// Field-wise merge of the two Auto-mode config tiers (config wins, remote fills
/// gaps, default otherwise). Pure so the precedence is unit-testable.
fn merge_auto_mode_config(
    config: crate::agent::config::AutoModeConfig,
    remote: crate::agent::config::AutoModeConfig,
) -> crate::agent::config::AutoModeConfig {
    crate::agent::config::AutoModeConfig {
        enabled: config.enabled.or(remote.enabled),
        prompt_type: config.prompt_type.or(remote.prompt_type),
        classifier_model: config.classifier_model.or(remote.classifier_model),
        classify_timeout_ms: config.classify_timeout_ms.or(remote.classify_timeout_ms),
        reasoning_effort: config.reasoning_effort.or(remote.reasoning_effort),
    }
}

/// Full Auto-mode config for the classifier-wiring read (overlay-free).
pub(crate) fn resolve_auto_mode_config_from_disk() -> crate::agent::config::AutoModeConfig {
    let config = match crate::config::ConfigLayers::load() {
        Ok(layers) => auto_mode_config_overlay_free(&layers),
        Err(_) => crate::agent::config::AutoModeConfig::default(),
    };
    let remote = REMOTE_AUTO_MODE_CONFIG
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    merge_auto_mode_config(config, remote)
}

pub(crate) fn auto_mode_classify_timeout(
    cfg: &crate::agent::config::AutoModeConfig,
) -> std::time::Duration {
    let configured = cfg
        .classify_timeout_ms
        .unwrap_or(AUTO_MODE_CLASSIFY_TIMEOUT_DEFAULT_MS);
    let bounded = configured.clamp(
        AUTO_MODE_CLASSIFY_TIMEOUT_MIN_MS,
        AUTO_MODE_CLASSIFY_TIMEOUT_MAX_MS,
    );
    if bounded != configured {
        tracing::warn!(
            configured_ms = configured,
            bounded_ms = bounded,
            min_ms = AUTO_MODE_CLASSIFY_TIMEOUT_MIN_MS,
            max_ms = AUTO_MODE_CLASSIFY_TIMEOUT_MAX_MS,
            "[auto_mode] classify_timeout_ms outside supported range; clamped"
        );
    }
    std::time::Duration::from_millis(bounded)
}

/// Apply the built-in Auto-mode classifier defaults to a resolved config (these
/// take effect once auto mode is enabled): an unset `prompt_type` defaults to
/// `full` (v9-traffic eval: transcript context cuts the residual block rate
/// ~1/3 and lets explicit user authorization satisfy the prompt's
/// confirmation clause); an unset `reasoning_effort` defaults to `low` ONLY
/// when the effective model supports reasoning effort (else stays `None` —
/// provider default). Explicit config/remote values always win. Returns the
/// `(prompt_type, reasoning_effort)` the classifier wiring should use.
pub(crate) fn auto_mode_classifier_defaults(
    cfg: &crate::agent::config::AutoModeConfig,
    effective_supports_reasoning_effort: bool,
) -> (
    echo_agent_workspace::permission::ClassifierPromptType,
    Option<echo_agent_sampling_types::ReasoningEffort>,
) {
    let prompt_type = cfg
        .prompt_type
        .unwrap_or(echo_agent_workspace::permission::ClassifierPromptType::Full);
    let reasoning_effort = cfg.reasoning_effort.or_else(|| {
        effective_supports_reasoning_effort
            .then_some(echo_agent_sampling_types::ReasoningEffort::Low)
    });
    (prompt_type, reasoning_effort)
}

#[cfg(test)]
mod auto_permission_mode_gate_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    // `ECHO_AGENT_AUTO_PERMISSION_MODE` is process-global; serialize every test that
    // reads it (all of them, via `BoolFlag::env`) and force it unset at the top
    // of each so a developer's shell value can't make these flaky.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = super::AUTO_PERMISSION_MODE_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
        g
    }

    fn toml_features_auto(v: bool) -> TomlValue {
        toml::from_str(&format!("[auto_mode]\nenabled = {v}\n")).unwrap()
    }

    fn remote(v: Option<bool>) -> RemoteSettings {
        RemoteSettings {
            auto_mode: v.map(|enabled| serde_json::json!({ "enabled": enabled })),
            ..RemoteSettings::default()
        }
    }

    #[test]
    fn defaults_on_when_nothing_set() {
        let _g = guard();
        let r = resolve_auto_permission_mode_enabled(None, None, None, None);
        assert!(r.value, "gate must default ON");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_on() {
        let _g = guard();
        let on = toml_features_auto(true);
        // requirement
        let r = resolve_auto_permission_mode_enabled(Some(&on), None, None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        // config (user)
        let r = resolve_auto_permission_mode_enabled(None, Some(&on), None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        // managed
        let r = resolve_auto_permission_mode_enabled(None, None, Some(&on), None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
        // remote settings (RemoteSettings field)
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&remote(Some(true))));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn remote_kill_switch_reads_struct_field() {
        let _g = guard();
        // Server explicitly disables → false from the Remote layer.
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&remote(Some(false))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Remote);
        // Absent remote field → falls through to Default ON.
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&remote(None)));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn remote_gate_coerces_json_object_else_default() {
        let _g = guard();
        // A well-formed lean `auto_mode` object yields the gate from `enabled`.
        let remote = RemoteSettings {
            auto_mode: Some(serde_json::json!({
                "enabled": true,
                "classifier_model": "some-slug",
                "prompt_type": "just_command"
            })),
            ..RemoteSettings::default()
        };
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&remote));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Remote);
        // A non-object / malformed payload coerces to None → falls through to Default ON.
        let bad = RemoteSettings {
            auto_mode: Some(serde_json::json!("not-an-object")),
            ..RemoteSettings::default()
        };
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&bad));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn remote_gate_malformed_field_falls_through_to_default() {
        let _g = guard();
        // A malformed field (bad `prompt_type` enum) drops the WHOLE object →
        // falls through to Default ON.
        let bad = RemoteSettings {
            auto_mode: Some(serde_json::json!({ "enabled": true, "prompt_type": "typo" })),
            ..RemoteSettings::default()
        };
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&bad));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Default);
        // A well-formed object still enables the gate from Remote.
        let ok = RemoteSettings {
            auto_mode: Some(serde_json::json!({ "enabled": true })),
            ..RemoteSettings::default()
        };
        let r = resolve_auto_permission_mode_enabled(None, None, None, Some(&ok));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Remote);
    }

    #[test]
    fn precedence_config_beats_managed_beats_remote() {
        let _g = guard();
        let off = toml_features_auto(false);
        let on = toml_features_auto(true);
        // config(false) wins over managed(true) and remote(true).
        let r = resolve_auto_permission_mode_enabled(
            None,
            Some(&off),
            Some(&on),
            Some(&remote(Some(true))),
        );
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        // managed(false) wins over remote(true) when no config.
        let r =
            resolve_auto_permission_mode_enabled(None, None, Some(&off), Some(&remote(Some(true))));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }

    #[test]
    fn env_overrides_config_and_remote() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_AUTO_PERMISSION_MODE, "1") };
        let off = toml_features_auto(false);
        let r = resolve_auto_permission_mode_enabled(
            None,
            Some(&off),
            None,
            Some(&remote(Some(false))),
        );
        assert!(r.value, "env must override config + remote");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_AUTO_PERMISSION_MODE, "1") };
        let off = toml_features_auto(false);
        let r = resolve_auto_permission_mode_enabled(Some(&off), None, None, None);
        assert!(!r.value, "requirement (managed/MDM floor) must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
    }

    #[test]
    fn remote_cache_round_trips_and_disk_reader_honors_env() {
        let _g = guard();
        // Gate `enabled` round-trips through the single RwLock store.
        cache_remote_auto_permission_mode_enabled(Some(true));
        assert_eq!(cached_remote_auto_permission_mode_enabled(), Some(true));
        cache_remote_auto_permission_mode_enabled(Some(false));
        assert_eq!(cached_remote_auto_permission_mode_enabled(), Some(false));
        cache_remote_auto_permission_mode_enabled(None);
        assert_eq!(cached_remote_auto_permission_mode_enabled(), None);
        // The disk reader wires the env layer (highest deterministic source).
        unsafe { std::env::set_var(ENV_AUTO_PERMISSION_MODE, "1") };
        assert!(
            auto_permission_mode_enabled_from_disk(),
            "from_disk must honor the env layer"
        );
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
        cache_remote_auto_mode(None);
    }

    #[test]
    fn auto_mode_config_is_overlay_free() {
        use echo_agent_workspace::permission::ClassifierPromptType;

        let user = crate::config::ConfigLayers {
            user: toml::from_str(
                "[auto_mode]\nenabled = false\nclassifier_model = \"trusted\"\nprompt_type = \"full\"\n",
            )
            .unwrap(),
            env_overlay: Some(
                toml::from_str(
                    "[auto_mode]\nenabled = true\nclassifier_model = \"rubber-stamp\"\nprompt_type = \"just_command\"\n",
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        let cfg = auto_mode_config_overlay_free(&user);
        assert_eq!(cfg.enabled, Some(false));
        assert_eq!(cfg.classifier_model.as_deref(), Some("trusted"));
        assert_eq!(cfg.prompt_type, Some(ClassifierPromptType::Full));

        let overlay_only = crate::config::ConfigLayers {
            env_overlay: Some(
                toml::from_str(
                    "[auto_mode]\nenabled = true\nclassifier_model = \"rubber-stamp\"\n",
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        let cfg = auto_mode_config_overlay_free(&overlay_only);
        assert_eq!(cfg.enabled, None);
        assert_eq!(cfg.classifier_model, None);
    }

    #[test]
    fn merge_auto_mode_config_precedence() {
        use crate::agent::config::AutoModeConfig;
        use echo_agent_sampling_types::ReasoningEffort;
        use echo_agent_workspace::permission::ClassifierPromptType;
        // config wins where set; remote fills the gaps.
        let config = AutoModeConfig {
            enabled: Some(true),
            prompt_type: Some(ClassifierPromptType::JustCommand),
            classifier_model: None,
            classify_timeout_ms: Some(45_000),
            reasoning_effort: None,
        };
        let remote = AutoModeConfig {
            enabled: Some(false),
            prompt_type: Some(ClassifierPromptType::Full),
            classifier_model: Some("remote-model".into()),
            classify_timeout_ms: Some(60_000),
            reasoning_effort: Some(ReasoningEffort::Low),
        };
        let merged = merge_auto_mode_config(config, remote);
        assert_eq!(merged.enabled, Some(true));
        assert_eq!(merged.prompt_type, Some(ClassifierPromptType::JustCommand));
        assert_eq!(merged.classifier_model.as_deref(), Some("remote-model"));
        assert_eq!(merged.classify_timeout_ms, Some(45_000));
        assert_eq!(merged.reasoning_effort, Some(ReasoningEffort::Low));
        let remote_timeout = merge_auto_mode_config(
            AutoModeConfig::default(),
            AutoModeConfig {
                classify_timeout_ms: Some(60_000),
                ..AutoModeConfig::default()
            },
        );
        assert_eq!(remote_timeout.classify_timeout_ms, Some(60_000));
        // Both unset ⇒ all-None (the wire fn then applies the built-in defaults).
        let empty = merge_auto_mode_config(AutoModeConfig::default(), AutoModeConfig::default());
        assert_eq!(empty.enabled, None);
        assert_eq!(empty.prompt_type, None);
        assert_eq!(empty.classifier_model, None);
        assert_eq!(empty.classify_timeout_ms, None);
        assert_eq!(empty.reasoning_effort, None);
    }

    #[test]
    fn auto_mode_classify_timeout_applies_default_and_bounds() {
        use crate::agent::config::AutoModeConfig;
        use std::time::Duration;

        assert_eq!(
            auto_mode_classify_timeout(&AutoModeConfig::default()),
            Duration::from_millis(AUTO_MODE_CLASSIFY_TIMEOUT_DEFAULT_MS)
        );
        assert_eq!(
            auto_mode_classify_timeout(&AutoModeConfig {
                classify_timeout_ms: Some(45_000),
                ..AutoModeConfig::default()
            }),
            Duration::from_millis(45_000)
        );
        assert_eq!(
            auto_mode_classify_timeout(&AutoModeConfig {
                classify_timeout_ms: Some(0),
                ..AutoModeConfig::default()
            }),
            Duration::from_millis(AUTO_MODE_CLASSIFY_TIMEOUT_MIN_MS)
        );
        assert_eq!(
            auto_mode_classify_timeout(&AutoModeConfig {
                classify_timeout_ms: Some(u64::MAX),
                ..AutoModeConfig::default()
            }),
            Duration::from_millis(AUTO_MODE_CLASSIFY_TIMEOUT_MAX_MS)
        );
    }

    #[test]
    fn auto_mode_classifier_defaults_apply_when_unset() {
        use crate::agent::config::AutoModeConfig;
        use echo_agent_sampling_types::ReasoningEffort;
        use echo_agent_workspace::permission::ClassifierPromptType;
        // Unset + RE-supporting effective model ⇒ full (transcript) + low.
        let (pt, eff) = auto_mode_classifier_defaults(&AutoModeConfig::default(), true);
        assert_eq!(pt, ClassifierPromptType::Full);
        assert_eq!(eff, Some(ReasoningEffort::Low));
        // Unset + non-RE model ⇒ full + None (no effort override).
        let (pt, eff) = auto_mode_classifier_defaults(&AutoModeConfig::default(), false);
        assert_eq!(pt, ClassifierPromptType::Full);
        assert_eq!(eff, None);
        // Explicit values win over the defaults, even on a RE-supporting model.
        let cfg = AutoModeConfig {
            prompt_type: Some(ClassifierPromptType::JustCommand),
            reasoning_effort: Some(ReasoningEffort::High),
            ..AutoModeConfig::default()
        };
        let (pt, eff) = auto_mode_classifier_defaults(&cfg, true);
        assert_eq!(pt, ClassifierPromptType::JustCommand);
        assert_eq!(eff, Some(ReasoningEffort::High));
    }

    #[test]
    fn auto_mode_config_from_toml_round_trips_and_warns_on_malformed() {
        use echo_agent_workspace::permission::ClassifierPromptType;
        // A real [auto_mode] table round-trips (not silently dropped).
        let toml: TomlValue = toml::from_str(
            "[auto_mode]\nenabled = true\nprompt_type = \"just_command\"\nclassifier_model = \"m\"\nclassify_timeout_ms = 45000\n",
        )
        .unwrap();
        let cfg = auto_mode_config_from_toml(Some(&toml)).expect("table parses");
        assert_eq!(cfg.enabled, Some(true));
        assert_eq!(cfg.prompt_type, Some(ClassifierPromptType::JustCommand));
        assert_eq!(cfg.classifier_model.as_deref(), Some("m"));
        assert_eq!(cfg.classify_timeout_ms, Some(45_000));
        // Absent [auto_mode] ⇒ None.
        let bare: TomlValue = toml::from_str("[features]\ngoal = true\n").unwrap();
        assert!(auto_mode_config_from_toml(Some(&bare)).is_none());
        // Malformed enum ⇒ dropped to None (warned), never a panic.
        let bad: TomlValue = toml::from_str("[auto_mode]\nprompt_type = \"bogus\"\n").unwrap();
        assert!(auto_mode_config_from_toml(Some(&bad)).is_none());
    }

    #[test]
    fn remote_cache_single_store_killswitch_preserves_fields() {
        use echo_agent_workspace::permission::ClassifierPromptType;
        let _g = guard();
        // Seed the full remote config, then flip ONLY the gate via the pager
        // kill-switch path — classifier fields must survive.
        cache_remote_auto_mode(Some(serde_json::json!({
            "enabled": true,
            "prompt_type": "bare_instructions",
            "classifier_model": "remote-model",
            "classify_timeout_ms": 45000
        })));
        assert_eq!(cached_remote_auto_permission_mode_enabled(), Some(true));
        cache_remote_auto_permission_mode_enabled(Some(false));
        assert_eq!(cached_remote_auto_permission_mode_enabled(), Some(false));
        let stored = REMOTE_AUTO_MODE_CONFIG
            .read()
            .unwrap()
            .clone()
            .expect("config still cached");
        assert_eq!(
            stored.prompt_type,
            Some(ClassifierPromptType::BareInstructions)
        );
        assert_eq!(stored.classifier_model.as_deref(), Some("remote-model"));
        assert_eq!(stored.classify_timeout_ms, Some(45_000));
        cache_remote_auto_mode(None);
    }
}
