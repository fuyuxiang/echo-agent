//! `ConfigLayers` and the layered config merge / overlay / campaign policy.
//!
//! The layer *files* are read by [`crate::loader`]; this module owns how those
//! layers combine into the effective config — layer precedence, the
//! `ECHO_AGENT_CONFIG` overlay, and campaign resolution.

use crate::loader::{
    deep_merge_toml, load_from_disk, load_managed_config, load_system_managed_config,
    normalize_config_layer,
};
use crate::validation::{load_requirements, load_system_requirements};

/// Whether a layer merge includes the `ECHO_AGENT_CONFIG` overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayInclusion {
    Include,
    Exclude,
}

/// Layers lowest→highest priority. `[[campaigns]]` taken off each layer at load.
#[derive(Clone)]
pub struct ConfigLayers {
    pub system_managed: toml::Value,
    pub managed: toml::Value,
    pub user: toml::Value,
    /// `ECHO_AGENT_CONFIG` / `ECHO_AGENT_CONFIG_PATH` overlay, above user but below
    /// requirements. Soft settings only, and the canonical source of truth for
    /// what the overlay can and cannot reach.
    ///
    /// Values are confined at [`crate::env_overlay`]'s `finalize_overlay` choke
    /// point. Both producers return confined overlays: `load_env_overlay` (the
    /// merge path, via [`Self::load`]) and `resolved_env_overlay` (hints, which
    /// constructs this field directly, and `echoagent inspect`). Construct this field
    /// only from one of those producers. Tests that assign an unconfined value do
    /// so deliberately, to exercise a gate independent of the allowlist.
    ///
    /// The overlay is confined to an allowlist of soft paths
    /// ([`crate::config_override::OVERLAY_ALLOW_PATHS`]): the `models` and
    /// `features` tables, a narrowed `toolset` (only `[toolset.bash]
    /// login_shell_capture` and the `[toolset.web_search]` domain lists), and a
    /// `shell_environment_policy` restricted to its filter fields (`inherit`,
    /// `exclude`, `include_only`, `ignore_default_excludes`). Every other table,
    /// plus the shell-env `set` field, is dropped at the choke point
    /// ([`crate::env_overlay`]'s finalize step). This is fail-closed: any
    /// code-exec, auth, egress, trust, or discovery table (`mcp_servers`,
    /// `auth_provider`, `model_providers`, `plugins`, `auth`, `echo_agent_com_config`,
    /// `endpoints`, `marketplace`, `feedback`, `voice`, `cli`, `ui`, the
    /// per-model `[model.<id>]` block, plus `version_overrides` / `campaigns`) is
    /// absent from the allowlist, so it is dropped by default and a newly added
    /// dangerous table stays out until it is explicitly allowlisted. The overlay
    /// therefore cannot spawn a new command sink, set auth policy, redirect
    /// egress, elevate trust, or add a discovery source. `shell_environment_policy`
    /// cannot inject an env value (`set` is dropped); its remaining fields only
    /// select among env names the launcher already controls, so relative to a
    /// lower layer they may loosen or tighten what a subprocess inherits but never
    /// introduce a value. The overlay cannot influence subprocess execution
    /// beyond the launcher's own env. A launcher that must add an env var sets it
    /// on the process directly.
    /// `sandbox` and `telemetry` are likewise not allowlisted (set them via
    /// `ECHO_AGENT_SANDBOX` / `OTEL_*`); `[features] telemetry` is the master switch
    /// and is allowlisted.
    ///
    /// Even on the allowlisted tables, security gates read overlay-free (the raw
    /// disk layers via [`Self::effective_config_base_without_overlay`], explicit
    /// per-layer values, or the raw config files; requirements/MDM clamp on
    /// top): permission mode, plan approval, auto permission mode plus its
    /// classifier, remember tool approvals, `remote_fetch`, managed-config fetch,
    /// marketplace `require_sha`, ZDR access, folder trust, `[permission]`
    /// allow/deny rules, and the `[cli]` version bounds.
    pub env_overlay: Option<toml::Value>,
    pub user_requirements: Option<toml::Value>,
    pub system_requirements: Option<toml::Value>,
    /// macOS MDM requirements; highest requirements tier when present.
    pub mdm_requirements: Option<toml::Value>,
    pub campaigns: crate::campaigns::CampaignOverrides,
}

impl Default for ConfigLayers {
    fn default() -> Self {
        Self {
            system_managed: toml::Value::Table(Default::default()),
            managed: toml::Value::Table(Default::default()),
            user: toml::Value::Table(Default::default()),
            env_overlay: None,
            user_requirements: None,
            system_requirements: None,
            mdm_requirements: None,
            campaigns: crate::campaigns::CampaignOverrides::default(),
        }
    }
}

impl ConfigLayers {
    pub fn load() -> std::io::Result<Self> {
        use crate::campaigns::{CampaignOverrides, take_campaign_entries};

        let mut system_managed = load_system_managed_config()?;
        let system_managed_campaigns = take_campaign_entries(&mut system_managed, "system_managed");

        let mut managed = load_managed_config()?;
        let managed_campaigns = take_campaign_entries(&mut managed, "managed");

        let mut user = load_from_disk()?;
        let user_campaigns = take_campaign_entries(&mut user, "user");

        let env_overlay = crate::env_overlay::load_env_overlay();

        let mut user_requirements = load_requirements();
        let mut system_requirements = load_system_requirements();
        let mut mdm_requirements = crate::validation::mdm_requirements_value();

        // Highest-authority requirements tier first: `merge_campaign_entries` is
        // first-id-wins, so a duplicate campaign id must resolve mdm > system >
        // user — matching the layer precedence in `effective_config_base` (where
        // mdm is merged last/highest).
        let mut requirements_campaigns = Vec::new();
        if let Some(ref mut req) = mdm_requirements {
            requirements_campaigns.extend(take_campaign_entries(req, "requirements"));
        }
        if let Some(ref mut req) = system_requirements {
            requirements_campaigns.extend(take_campaign_entries(req, "requirements"));
        }
        if let Some(ref mut req) = user_requirements {
            requirements_campaigns.extend(take_campaign_entries(req, "requirements"));
        }

        // Normalize each layer before it is ever merged, so `[toolset.web_search]`'s
        // mutually-exclusive `allowed_domains` / `excluded_domains` travel together:
        // a layer that sets one clears the other to `[]`. That makes the existing
        // `deep_merge_toml` replace the whole policy from the winning layer instead
        // of mixing keys across layers.
        normalize_config_layer(&mut system_managed);
        normalize_config_layer(&mut managed);
        normalize_config_layer(&mut user);
        for req in [
            &mut user_requirements,
            &mut system_requirements,
            &mut mdm_requirements,
        ]
        .into_iter()
        .flatten()
        {
            normalize_config_layer(req);
        }

        Ok(Self {
            system_managed,
            managed,
            user,
            env_overlay,
            user_requirements,
            system_requirements,
            mdm_requirements,
            campaigns: CampaignOverrides {
                requirements: requirements_campaigns,
                user: user_campaigns,
                managed: managed_campaigns,
                system_managed: system_managed_campaigns,
            },
        })
    }

    /// Layer merge (no campaigns), including the `ECHO_AGENT_CONFIG` overlay.
    ///
    /// Overlay-inclusive: security gates must not read this. Use
    /// [`Self::effective_config_base_without_overlay`] for any gate (the
    /// overlay-free set is enumerated on [`Self::env_overlay`]).
    pub fn effective_config_base(&self) -> toml::Value {
        self.merge(OverlayInclusion::Include)
    }

    /// Layer merge excluding the `ECHO_AGENT_CONFIG` overlay, for security gates.
    pub fn effective_config_base_without_overlay(&self) -> toml::Value {
        self.merge(OverlayInclusion::Exclude)
    }

    fn merge(&self, inclusion: OverlayInclusion) -> toml::Value {
        let Self {
            system_managed,
            managed,
            user,
            env_overlay,
            user_requirements: _,
            system_requirements: _,
            mdm_requirements: _,
            campaigns: _,
        } = self;
        let mut merged = system_managed.clone();
        deep_merge_toml(&mut merged, managed);
        deep_merge_toml(&mut merged, user);
        if let (OverlayInclusion::Include, Some(overlay)) = (inclusion, env_overlay) {
            deep_merge_toml(&mut merged, overlay);
        }
        for req in self.requirements_in_order() {
            deep_merge_toml(&mut merged, req);
        }
        merged
    }

    fn requirements_in_order(&self) -> impl Iterator<Item = &toml::Value> {
        [
            &self.user_requirements,
            &self.system_requirements,
            &self.mdm_requirements,
        ]
        .into_iter()
        .flatten()
    }

    /// Campaign source slices in priority order (first id wins):
    /// requirements > remote > user > managed > system_managed. Single source of
    /// truth for the precedence; both this crate and the shell resolver consume it.
    pub fn campaign_source_slices<'a>(
        &'a self,
        remote_campaigns: &'a [crate::campaigns::CampaignEntry],
    ) -> [&'a [crate::campaigns::CampaignEntry]; 5] {
        [
            &self.campaigns.requirements,
            remote_campaigns,
            &self.campaigns.user,
            &self.campaigns.managed,
            &self.campaigns.system_managed,
        ]
    }

    /// Active campaigns against `base`: kill switch → priority merge (first-id-wins)
    /// → drop dismissed. The single place disk campaign resolution lives; the shell
    /// wraps this with the `ECHO_AGENT_CAMPAIGNS_OVERRIDE` env layer.
    pub fn resolve_campaigns(
        &self,
        base: &toml::Value,
        remote_campaigns: &[crate::campaigns::CampaignEntry],
        dismissed_ids: &std::collections::HashSet<String>,
    ) -> Vec<crate::campaigns::CampaignEntry> {
        if campaigns_application_disabled(base) {
            return Vec::new();
        }
        let merged = crate::campaigns::merge_campaign_entries(
            &self.campaign_source_slices(remote_campaigns),
        );
        crate::campaigns::filter_active_campaigns(merged, dismissed_ids)
    }

    /// Re-merge the requirements layers so an admin's `requirements.toml` always
    /// wins over a campaign overlay, regardless of the campaign's source layer.
    /// Campaigns are full-power (any field), so this is the structural guarantee
    /// that a lower-trust layer's campaign can't override an admin-set field.
    fn reapply_requirements(&self, merged: &mut toml::Value) {
        for req in self.requirements_in_order() {
            deep_merge_toml(merged, req);
        }
    }

    /// Apply campaign patches, re-apply the `ECHO_AGENT_CONFIG` overlay, then restore requirements.
    pub fn apply_campaign_overrides(
        &self,
        merged: &mut toml::Value,
        active: &[crate::campaigns::CampaignEntry],
    ) {
        crate::campaigns::apply_active_campaign_patches(merged, active);
        if let Some(overlay) = &self.env_overlay {
            deep_merge_toml(merged, overlay);
        }
        self.reapply_requirements(merged);
    }

    /// Layer merge + disk/remote campaign overlay, honoring the kill switch. The
    /// shell's `load_effective_config` is the remote/override-aware path; this is
    /// used by `effective_config_disk_only` and tests.
    pub fn effective_config_with_campaigns(
        &self,
        remote_campaigns: &[crate::campaigns::CampaignEntry],
        dismissed_ids: &std::collections::HashSet<String>,
    ) -> toml::Value {
        let mut merged = self.effective_config_base();
        let active = self.resolve_campaigns(&merged, remote_campaigns, dismissed_ids);
        self.apply_campaign_overrides(&mut merged, &active);
        merged
    }

    /// Disk campaigns + on-disk dismiss (`campaigns_state.json`); **no remote, no
    /// env override**. Named to make the divergence from the shell's remote-aware
    /// `load_effective_config` explicit at every call site.
    pub fn effective_config_disk_only(&self) -> toml::Value {
        self.effective_config_with_campaigns(&[], &load_dismissed_ids_from_home())
    }

    pub fn has_managed(&self) -> bool {
        self.managed.as_table().is_some_and(|t| !t.is_empty())
            || self
                .system_managed
                .as_table()
                .is_some_and(|t| !t.is_empty())
    }

    pub fn has_system_managed(&self) -> bool {
        self.system_managed
            .as_table()
            .is_some_and(|t| !t.is_empty())
    }
}

/// `ECHO_AGENT_CAMPAIGNS=0` or `[features] campaigns = false` on pre-campaign base.
pub fn campaigns_application_disabled(base_effective: &toml::Value) -> bool {
    if crate::env_bool("ECHO_AGENT_CAMPAIGNS") == Some(false) {
        return true;
    }
    base_effective
        .get("features")
        .and_then(|f| f.get("campaigns"))
        .and_then(|c| c.as_bool())
        == Some(false)
}

/// Disk layers only (no remote, no env override). Prefer the shell loader
/// (`echo_agent_runtime::util::config::load_effective_config`) when remote campaigns
/// or `ECHO_AGENT_CAMPAIGNS_OVERRIDE` must be honored. The name mirrors the
/// [`ConfigLayers::effective_config_disk_only`] method so the divergence from the
/// remote-aware loader is un-ignorable at every call site.
pub fn load_effective_config_disk_only() -> std::io::Result<toml::Value> {
    Ok(ConfigLayers::load()?.effective_config_disk_only())
}

/// On-disk campaign dismiss state. Single source of truth for the file's name,
/// location, and JSON shape — the shell's writer reuses these so the read and
/// write sides can't drift.
pub const CAMPAIGNS_STATE_FILE: &str = "campaigns_state.json";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CampaignsState {
    #[serde(default)]
    pub dismissed_ids: Vec<String>,
}

/// Path to `$ECHO_AGENT_HOME/campaigns_state.json` under `home`.
pub fn campaigns_state_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(CAMPAIGNS_STATE_FILE)
}

/// Fail-open dismissed ids from `$ECHO_AGENT_HOME/campaigns_state.json`.
pub fn load_dismissed_ids_from_home() -> std::collections::HashSet<String> {
    let Some(home) = crate::user_echo_agent_home() else {
        return std::collections::HashSet::new();
    };
    let Ok(contents) = std::fs::read_to_string(campaigns_state_path(&home)) else {
        return std::collections::HashSet::new();
    };
    serde_json::from_str::<CampaignsState>(&contents)
        .map(|s| s.dismissed_ids.into_iter().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::version_overrides::apply_version_overrides;

    #[test]
    fn full_layer_precedence_requirements_over_config_over_managed() {
        let system_managed: toml::Value =
            toml::from_str("[telemetry]\nmode = \"system_managed_value\"\n").unwrap();
        let managed: toml::Value =
            toml::from_str("[telemetry]\nmode = \"managed_value\"\n").unwrap();
        let user: toml::Value = toml::from_str("[telemetry]\nmode = \"user_value\"\n").unwrap();
        let user_requirements: toml::Value =
            toml::from_str("[telemetry]\nmode = \"user_requirements_value\"\n").unwrap();
        let system_requirements: toml::Value =
            toml::from_str("[telemetry]\nmode = \"system_requirements_value\"\n").unwrap();

        let mut merged = system_managed;
        deep_merge_toml(&mut merged, &managed);
        deep_merge_toml(&mut merged, &user);
        deep_merge_toml(&mut merged, &user_requirements);
        deep_merge_toml(&mut merged, &system_requirements);

        assert_eq!(
            merged["telemetry"]["mode"].as_str(),
            Some("system_requirements_value")
        );
    }

    #[test]
    fn effective_config_mdm_requirements_win_over_system_and_user() {
        // MDM is merged last, so an admin-forced value clamps the effective
        // config over both the user config and the system requirements layer.
        let layers = ConfigLayers {
            user: toml::from_str("[features]\nweb_fetch = true\n").unwrap(),
            system_requirements: Some(toml::from_str("[features]\nweb_fetch = true\n").unwrap()),
            mdm_requirements: Some(toml::from_str("[features]\nweb_fetch = false\n").unwrap()),
            ..Default::default()
        };
        assert_eq!(
            layers.effective_config_disk_only()["features"]["web_fetch"].as_bool(),
            Some(false),
        );
    }

    #[test]
    fn full_precedence_holds_when_values_come_from_version_overrides() {
        let cli_version = semver::Version::parse("1.8.0").unwrap();

        let mut managed: toml::Value = toml::from_str(
            r#"
            [[version_overrides]]
            minimum_version = "1.0.0"
            [version_overrides.telemetry]
            mode = "managed_versioned"
            "#,
        )
        .unwrap();
        let mut user: toml::Value = toml::from_str(
            r#"
            [[version_overrides]]
            minimum_version = "1.0.0"
            [version_overrides.telemetry]
            mode = "user_versioned"
            "#,
        )
        .unwrap();
        let mut requirements: toml::Value = toml::from_str(
            r#"
            [[version_overrides]]
            minimum_version = "1.0.0"
            [version_overrides.telemetry]
            mode = "requirements_versioned"
            "#,
        )
        .unwrap();

        apply_version_overrides(&mut managed, &cli_version).unwrap();
        apply_version_overrides(&mut user, &cli_version).unwrap();
        apply_version_overrides(&mut requirements, &cli_version).unwrap();

        let mut merged = managed;
        deep_merge_toml(&mut merged, &user);
        deep_merge_toml(&mut merged, &requirements);

        assert_eq!(
            merged["telemetry"]["mode"].as_str(),
            Some("requirements_versioned")
        );
    }

    /// `ECHO_AGENT_CAMPAIGNS=0` disables campaign application regardless of config.
    /// `ECHO_AGENT_CAMPAIGNS` is process-global, so this test serializes itself with a
    /// module-local mutex and save/restores the prior value. (This crate has no
    /// `serial_test` dev-dep and no other test reads this var, so a local guard
    /// is sufficient.)
    #[test]
    fn kill_switch_env_var_disables() {
        static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var_os("ECHO_AGENT_CAMPAIGNS");
        let empty = toml::Value::Table(Default::default());

        // SAFETY: ENV_GUARD serializes this against itself; no other test in the
        // crate mutates or reads ECHO_AGENT_CAMPAIGNS concurrently.
        unsafe { std::env::set_var("ECHO_AGENT_CAMPAIGNS", "0") };
        assert!(campaigns_application_disabled(&empty));

        unsafe { std::env::remove_var("ECHO_AGENT_CAMPAIGNS") };
        assert!(!campaigns_application_disabled(&empty));

        match prior {
            Some(v) => unsafe { std::env::set_var("ECHO_AGENT_CAMPAIGNS", v) },
            None => unsafe { std::env::remove_var("ECHO_AGENT_CAMPAIGNS") },
        }
    }

    #[test]
    fn env_overlay_precedence_and_overlay_free_merge() {
        let mut layers = ConfigLayers {
            user: toml::from_str("[models]\ndefault = \"user\"\n[telemetry]\nmode = \"on\"\n")
                .unwrap(),
            env_overlay: Some(
                toml::from_str(
                    "[models]\ndefault = \"overlay\"\ndefault_reasoning_effort = \"high\"\n",
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        layers.campaigns.managed = vec![crate::campaigns::CampaignEntry {
            id: "c1".into(),
            patch: toml::from_str("[models]\ndefault = \"campaign\"\n").unwrap(),
        }];
        let none = std::collections::HashSet::new();

        let with_overlay: toml::Value = toml::from_str(
            "[models]\ndefault = \"overlay\"\ndefault_reasoning_effort = \"high\"\n\
             [telemetry]\nmode = \"on\"\n",
        )
        .unwrap();
        assert_eq!(
            layers.effective_config_with_campaigns(&[], &none),
            with_overlay
        );

        let overlay_free: toml::Value =
            toml::from_str("[models]\ndefault = \"user\"\n[telemetry]\nmode = \"on\"\n").unwrap();
        assert_eq!(layers.effective_config_base_without_overlay(), overlay_free);

        layers.user_requirements =
            Some(toml::from_str("[models]\ndefault = \"pinned\"\n").unwrap());
        let clamped: toml::Value = toml::from_str(
            "[models]\ndefault = \"pinned\"\ndefault_reasoning_effort = \"high\"\n\
             [telemetry]\nmode = \"on\"\n",
        )
        .unwrap();
        assert_eq!(layers.effective_config_with_campaigns(&[], &none), clamped);
    }
}
