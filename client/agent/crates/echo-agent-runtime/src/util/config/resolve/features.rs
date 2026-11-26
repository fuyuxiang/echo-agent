use crate::util::config::RemoteSettings;
use toml::Value as TomlValue;

/// Resolve whether ZDR users are allowed to use the product.
///
/// Precedence: requirements > env > config.toml > managed > remote settings > default (false).
pub fn resolve_zdr_access_enabled(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    remote: Option<&RemoteSettings>,
) -> bool {
    use crate::agent::config::BoolFlag;
    fn from_toml(v: Option<&TomlValue>) -> Option<bool> {
        v?.get("features")?.get("zdr_access_enabled")?.as_bool()
    }
    BoolFlag::env("ECHO_AGENT_ZDR_ACCESS_ENABLED")
        .requirement(from_toml(requirements))
        .config(from_toml(user))
        .managed(from_toml(managed))
        .feature_flag(remote.and_then(|r| r.zdr_access_enabled))
        .resolve()
        .value
}

/// Whether model-catalog (`/v1/models`) and remote-settings (`/v1/settings`)
/// fetches from EchoAgent backends are allowed, including the deployment-config sync
/// bundled into the startup prefetch (the background managed-config sync has
/// its own `[features] managed_config` gate).
///
/// Precedence: requirements (MDM > system > user) > managed
/// (`managed_config.toml` > system managed) > user `config.toml` > default
/// (true). Overlay-free: the `ECHO_AGENT_CONFIG` / `ECHO_AGENT_CONFIG_PATH` overlay is
/// deliberately excluded (an egress gate, matching the overlay-free contract in
/// `ConfigLayers::env_overlay`), so an overlay cannot re-arm a user's or a
/// deployment's "never fetch" decision. Callable before an `AgentConfig` exists
/// (startup prefetch runs pre-agent), so it re-reads the config layers like
/// `managed_config::is_fetch_enabled`.
///
/// Deliberately no env var and no remote tier: remote settings are exactly
/// what is unreachable when this knob is needed (firewalled / air-gapped
/// deployments), and an env var would be one more way to re-arm the fetches.
pub fn resolve_remote_fetch_enabled() -> bool {
    match crate::config::ConfigLayers::load() {
        Ok(layers) => remote_fetch_enabled_from_layers(&layers),
        // The full-layer load is all-or-nothing, but the policy tiers load
        // independently (requirements soft-fail per layer; the managed loaders
        // are the same ones ConfigLayers::load uses) — a corrupt user-writable
        // config.toml must not disarm a requirements or managed-layer pin.
        // Fail open only when policy is genuinely absent.
        Err(_) => remote_fetch_enabled_from_policy_layers(
            crate::config::load_merged_requirements().as_ref(),
            crate::config::load_managed_config().ok().as_ref(),
            crate::config::load_system_managed_config().ok().as_ref(),
        ),
    }
}

pub const REMOTE_FETCH_CONFIG_PATH: &str = "features.remote_fetch";

/// Keys whose dedicated resolver walks managed before user `config.toml`.
/// The effective merge lets the user file win for every other key.
pub const MANAGED_WINS_OVER_USER: &[&str] = &[REMOTE_FETCH_CONFIG_PATH];

fn remote_fetch_value(v: &TomlValue) -> Option<bool> {
    v.get("features")?.get("remote_fetch")?.as_bool()
}

/// First-match layer walk instead of the plain effective-config merge: the
/// merge puts the user layer over managed, but for this knob the management
/// layer must win so a user's stray `remote_fetch = true` cannot re-arm a
/// deployment's "never fetch" decision.
fn remote_fetch_enabled_from_layers(layers: &crate::config::ConfigLayers) -> bool {
    // Exhaustive destructure (no `..`): a future layer must be slotted into the
    // walk deliberately instead of silently keeping stale precedence.
    // `env_overlay` is deliberately NOT in the walk: the `ECHO_AGENT_CONFIG` overlay
    // is soft, user-tier input, and this is an egress gate, so it must never
    // arm/disarm remote_fetch (the overlay-free contract in
    // `ConfigLayers::env_overlay`). `campaigns` is excluded for the same reason:
    // campaign patches are soft, dismissable overlays applied after the layer
    // merge, and requirements are re-merged over campaigns for the same reason.
    let crate::config::ConfigLayers {
        system_managed,
        managed,
        user,
        env_overlay: _,
        user_requirements,
        system_requirements,
        mdm_requirements,
        campaigns: _,
    } = layers;
    [
        mdm_requirements.as_ref(),
        system_requirements.as_ref(),
        user_requirements.as_ref(),
        Some(managed),
        Some(system_managed),
        Some(user),
    ]
    .into_iter()
    .flatten()
    .find_map(remote_fetch_value)
    .unwrap_or(true)
}

/// Err-arm fallback for [`resolve_remote_fetch_enabled`]: the independently
/// loadable policy tiers in Ok-arm walk order — merged requirements
/// (`load_merged_requirements` merges user, system, MDM with last-wins,
/// matching the walk), then the managed tiers — so a root-owned or synced
/// managed-only pin also survives a corrupt user layer. The user `config.toml`
/// tier stays fail-open: it is a preference, not deployment policy. Mirrors
/// the `auto_permission_mode_enabled_from_disk` soft-fail precedent.
fn remote_fetch_enabled_from_policy_layers(
    merged_requirements: Option<&TomlValue>,
    managed: Option<&TomlValue>,
    system_managed: Option<&TomlValue>,
) -> bool {
    [merged_requirements, managed, system_managed]
        .into_iter()
        .flatten()
        .find_map(remote_fetch_value)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ConfigLayers;

    fn empty_layers() -> ConfigLayers {
        ConfigLayers {
            system_managed: TomlValue::Table(Default::default()),
            managed: TomlValue::Table(Default::default()),
            user: TomlValue::Table(Default::default()),
            user_requirements: None,
            system_requirements: None,
            mdm_requirements: None,
            ..Default::default()
        }
    }

    fn features_remote_fetch(v: bool) -> TomlValue {
        toml::from_str(&format!("[features]\nremote_fetch = {v}\n")).unwrap()
    }

    #[test]
    fn remote_fetch_defaults_to_true_when_absent() {
        assert!(remote_fetch_enabled_from_layers(&empty_layers()));
    }

    #[test]
    fn remote_fetch_reads_user_config() {
        let mut layers = empty_layers();
        layers.user = features_remote_fetch(false);
        assert!(!remote_fetch_enabled_from_layers(&layers));
    }

    #[test]
    fn remote_fetch_managed_overrides_user() {
        let mut layers = empty_layers();
        layers.user = features_remote_fetch(true);
        layers.managed = features_remote_fetch(false);
        assert!(
            !remote_fetch_enabled_from_layers(&layers),
            "managed=false must beat user=true"
        );

        // Both directions, proving precedence rather than AND-ing.
        layers.user = features_remote_fetch(false);
        layers.managed = features_remote_fetch(true);
        assert!(
            remote_fetch_enabled_from_layers(&layers),
            "managed=true must beat user=false"
        );
    }

    #[test]
    fn remote_fetch_requirements_pin_beats_managed_and_user() {
        let mut layers = empty_layers();
        layers.user = features_remote_fetch(true);
        layers.managed = features_remote_fetch(true);
        layers.user_requirements = Some(features_remote_fetch(false));
        assert!(
            !remote_fetch_enabled_from_layers(&layers),
            "requirements=false must beat managed and user"
        );

        layers.user = features_remote_fetch(false);
        layers.managed = features_remote_fetch(false);
        layers.user_requirements = Some(features_remote_fetch(true));
        assert!(
            remote_fetch_enabled_from_layers(&layers),
            "requirements=true must beat managed and user"
        );
    }

    #[test]
    fn remote_fetch_env_overlay_is_ignored_in_both_directions() {
        let mut layers = empty_layers();
        layers.user = features_remote_fetch(false);
        layers.env_overlay = Some(features_remote_fetch(true));
        assert!(!remote_fetch_enabled_from_layers(&layers));

        let mut layers = empty_layers();
        layers.env_overlay = Some(features_remote_fetch(false));
        assert!(remote_fetch_enabled_from_layers(&layers));
    }

    #[test]
    fn remote_fetch_system_and_mdm_tiers_follow_the_walk() {
        // Within the managed tier: user-level managed_config.toml beats the
        // system managed layer (mirrors effective_config merge order), and
        // system managed still beats the user config.
        let mut layers = empty_layers();
        layers.system_managed = features_remote_fetch(true);
        layers.managed = features_remote_fetch(false);
        assert!(!remote_fetch_enabled_from_layers(&layers));
        layers.managed = features_remote_fetch(true);
        layers.system_managed = features_remote_fetch(false);
        assert!(remote_fetch_enabled_from_layers(&layers));
        let mut layers = empty_layers();
        layers.user = features_remote_fetch(true);
        layers.system_managed = features_remote_fetch(false);
        assert!(!remote_fetch_enabled_from_layers(&layers));

        // Within the requirements tier: system beats user requirements, MDM
        // beats both (mirrors requirements_layers apply order).
        let mut layers = empty_layers();
        layers.user_requirements = Some(features_remote_fetch(true));
        layers.system_requirements = Some(features_remote_fetch(false));
        assert!(!remote_fetch_enabled_from_layers(&layers));
        layers.mdm_requirements = Some(features_remote_fetch(true));
        assert!(remote_fetch_enabled_from_layers(&layers));
        layers.mdm_requirements = Some(features_remote_fetch(false));
        layers.system_requirements = Some(features_remote_fetch(true));
        assert!(!remote_fetch_enabled_from_layers(&layers));
    }

    /// The all-or-nothing layer load failing (corrupt user config.toml, IO
    /// error) must not disarm a policy pin — the Err arm still consults the
    /// merged requirements and both managed tiers, in Ok-arm walk order, and
    /// fails open only with no policy at all.
    #[test]
    fn remote_fetch_layer_load_failure_still_honors_policy_pins() {
        let off = features_remote_fetch(false);
        let on = features_remote_fetch(true);
        // Requirements pin survives, both directions.
        assert!(!remote_fetch_enabled_from_policy_layers(
            Some(&off),
            None,
            None
        ));
        assert!(remote_fetch_enabled_from_policy_layers(
            Some(&on),
            None,
            None
        ));
        // A pin living only in a managed tier survives too (root-owned
        // system-managed-only and synced managed-only deployments).
        assert!(!remote_fetch_enabled_from_policy_layers(
            None,
            None,
            Some(&off)
        ));
        assert!(!remote_fetch_enabled_from_policy_layers(
            None,
            Some(&off),
            None
        ));
        // Precedence mirrors the Ok-arm walk: requirements > managed > system managed.
        assert!(remote_fetch_enabled_from_policy_layers(
            Some(&on),
            Some(&off),
            Some(&off)
        ));
        assert!(!remote_fetch_enabled_from_policy_layers(
            None,
            Some(&off),
            Some(&on)
        ));
        assert!(
            remote_fetch_enabled_from_policy_layers(None, None, None),
            "genuinely absent policy fails open"
        );
    }
}
