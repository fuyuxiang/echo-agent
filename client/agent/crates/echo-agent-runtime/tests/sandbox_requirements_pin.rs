//! Verify `requirements.toml` can pin the base sandbox `profile`.

use echo_agent_runtime::agent::config::{ConfigSource, SandboxSettingsConfig};

#[test]
fn requirements_pin_profile() {
    let config = SandboxSettingsConfig::default();
    let resolved = config.resolve_profile(None, Some("strict"));
    assert_eq!(resolved.value, "strict");
    assert_eq!(resolved.source, ConfigSource::Requirement);
}

#[test]
fn cli_flag_overrides_config_but_not_requirement() {
    let config = SandboxSettingsConfig {
        profile: Some("workspace".to_string()),
        ..Default::default()
    };
    let resolved = config.resolve_profile(Some("read-only"), Some("strict"));
    assert_eq!(resolved.value, "strict");
    assert_eq!(resolved.source, ConfigSource::Requirement);
}
