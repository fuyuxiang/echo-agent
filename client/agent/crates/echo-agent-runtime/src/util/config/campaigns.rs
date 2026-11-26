//! Campaign dismiss state, remote cache, and effective-config overlay.
//!
//! Design, invariants, and the "adding a second governed field" recipe are
//! documented alongside this module.

use std::collections::HashSet;
use std::path::Path;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use echo_agent_config::campaigns::{
    CampaignEntry, filter_active_campaigns, ids_touching_paths, merge_campaign_entries,
};
use echo_agent_config::config_override::{PatchPath, patch_touches_any};
use echo_agent_config::{
    CampaignsState, ConfigLayers, campaigns_state_path, load_dismissed_ids_from_home,
    user_echo_agent_home,
};
use echo_agent_config_types::{CampaignOverride, RemoteSettings};

/// FIFO cap on persisted dismissed ids; evicting the oldest can re-nudge for a
/// still-live campaign after a user dismisses more than this over the CLI's life.
const MAX_DISMISSED_IDS: usize = 32;

static DISMISS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static DISMISS_TMP_NONCE: AtomicU64 = AtomicU64::new(0);

static REMOTE_CAMPAIGN_CACHE: RwLock<Vec<CampaignEntry>> = RwLock::new(Vec::new());

/// Seed the process-global remote campaign cache. A `None` settings value (e.g.
/// a failed fetch) is a no-op so it can't clobber a previously-seeded cache;
/// `Some` with zero campaigns legitimately clears it (campaigns withdrawn).
pub fn set_remote_campaigns_from_settings(remote: Option<&RemoteSettings>) {
    let Some(remote) = remote else {
        return;
    };
    set_remote_campaigns(remote_campaigns_from_settings(Some(remote)));
}

fn set_remote_campaigns(entries: Vec<CampaignEntry>) {
    if let Ok(mut g) = REMOTE_CAMPAIGN_CACHE.write() {
        *g = entries;
    }
}

fn cached_remote_campaigns() -> Vec<CampaignEntry> {
    REMOTE_CAMPAIGN_CACHE
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Fail-open dismissed campaign ids from `campaigns_state.json`.
pub(crate) fn load_dismissed_ids() -> HashSet<String> {
    load_dismissed_ids_from_home()
}

pub(crate) fn dismiss_campaign_ids(ids: impl IntoIterator<Item = String>) {
    let Some(home) = user_echo_agent_home() else {
        return;
    };
    if let Err(e) = dismiss_campaign_ids_at(&home, ids) {
        tracing::warn!(error = %e, "campaigns: failed to persist dismiss state");
    }
}

/// Append `ids` to the dismissed set and write `campaigns_state.json` atomically
/// (temp + rename). Corrupt prior state is renamed aside, not discarded.
fn dismiss_campaign_ids_at(
    home: &Path,
    ids: impl IntoIterator<Item = String>,
) -> std::io::Result<()> {
    use fs2::FileExt as _;
    let _guard = DISMISS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = campaigns_state_path(home);
    // Cross-process advisory lock over the read-modify-write: in leader mode
    // several echoagent processes share `$ECHO_AGENT_HOME`; the in-process mutex alone would
    // let them lose-update the set. Best-effort; a lock failure still proceeds.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"));
    if let Ok(ref f) = lock {
        let _ = f.lock_exclusive();
    }
    let mut ordered = match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<CampaignsState>(&contents) {
            Ok(s) => s.dismissed_ids,
            Err(e) => {
                let _ = std::fs::rename(&path, path.with_extension("json.corrupt"));
                tracing::warn!(error = %e, "campaigns: corrupt dismiss state; renamed aside");
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut seen: HashSet<String> = ordered.iter().cloned().collect();
    for id in ids {
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        ordered.push(id);
    }
    if ordered.len() > MAX_DISMISSED_IDS {
        let drop_n = ordered.len() - MAX_DISMISSED_IDS;
        ordered.drain(..drop_n);
    }
    let json = serde_json::to_string(&CampaignsState {
        dismissed_ids: ordered,
    })
    .map_err(std::io::Error::other)?;
    let nonce = DISMISS_TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{}.tmp", std::process::id(), nonce));
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// `ECHO_AGENT_CAMPAIGNS_OVERRIDE` JSON array replaces all sources (`[]` = none; beats
/// kill switch). Invalid JSON also resolves to none: the var's intent is "replace
/// campaigns with exactly this", so a typo must not silently fall back to the
/// real sources it was meant to replace.
pub(crate) fn campaigns_override() -> Option<Vec<CampaignEntry>> {
    let json = std::env::var("ECHO_AGENT_CAMPAIGNS_OVERRIDE").ok()?;
    match serde_json::from_str::<Vec<CampaignOverride>>(&json) {
        Ok(list) => Some(
            list.into_iter()
                .filter_map(remote_campaign_to_entry)
                .collect(),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "invalid ECHO_AGENT_CAMPAIGNS_OVERRIDE JSON; suppressing all campaigns");
            Some(Vec::new())
        }
    }
}

fn remote_campaign_to_entry(c: CampaignOverride) -> Option<CampaignEntry> {
    let id = c.id.as_deref()?.trim();
    if id.is_empty() {
        return None;
    }
    let id = id.to_owned();
    // Full-power patch (any field); no allowlist filtering — requirements
    // precedence is restored by `ConfigLayers::apply_campaign_overrides`.
    let patch = match toml::Value::try_from(serde_json::Value::Object(c.patch)) {
        Ok(toml::Value::Table(t)) => t,
        Ok(_) => return None,
        Err(e) => {
            tracing::warn!(error = %e, %id, "campaigns: invalid remote patch; ignoring");
            return None;
        }
    };
    if patch.is_empty() {
        return None;
    }
    Some(CampaignEntry { id, patch })
}

pub fn remote_campaigns_from_settings(remote: Option<&RemoteSettings>) -> Vec<CampaignEntry> {
    remote
        .map(|rs| {
            rs.campaigns
                .iter()
                .cloned()
                .filter_map(remote_campaign_to_entry)
                .collect()
        })
        .unwrap_or_default()
}

/// The single campaign-resolution path: `ECHO_AGENT_CAMPAIGNS_OVERRIDE` (replaces all
/// sources and beats the kill switch) → kill switch → layer+remote merge →
/// dismiss. `base` is the pre-campaign effective config, used only for the
/// kill-switch check.
pub(crate) fn resolve_active_campaigns_from_layers(
    layers: &ConfigLayers,
    base: &toml::Value,
    remote_entries: &[CampaignEntry],
    dismissed: &HashSet<String>,
) -> Vec<CampaignEntry> {
    if let Some(over) = campaigns_override() {
        return filter_active_campaigns(over, dismissed);
    }
    layers.resolve_campaigns(base, remote_entries, dismissed)
}

/// Campaigns eligible for dismissal when the user persists a choice (loads
/// layers + remote cache + dismiss state).
///
/// Unlike the apply path this deliberately **ignores the kill switch**:
/// dismissing a suppressed campaign is harmless, while skipping the dismissal
/// lets a later re-enabled campaign override a choice the user already made
/// ("user pick wins, forever"). A layer-load failure likewise falls back to
/// the remote cache instead of failing closed — remote campaigns still get
/// dismissed on that path, though disk-layer campaigns can be missed until
/// the transient failure clears (they re-dismiss on the next pick).
fn resolve_dismissable_campaigns() -> Vec<CampaignEntry> {
    let dismissed = load_dismissed_ids();
    if let Some(over) = campaigns_override() {
        return filter_active_campaigns(over, &dismissed);
    }
    let remote_entries = cached_remote_campaigns();
    match ConfigLayers::load() {
        Ok(layers) => filter_active_campaigns(
            merge_campaign_entries(&layers.campaign_source_slices(&remote_entries)),
            &dismissed,
        ),
        Err(e) => {
            tracing::warn!(error = %e, "campaigns: layer load failed; dismiss bookkeeping using remote cache only");
            filter_active_campaigns(remote_entries, &dismissed)
        }
    }
}

/// Effective config with remote/override-aware campaign overlay
/// (base → resolve [override/kill/merge/dismiss] → apply), one `ConfigLayers::load`.
pub fn load_effective_config() -> std::io::Result<toml::Value> {
    let mut layers = ConfigLayers::load()?;
    if let Some(servers) = layers
        .user
        .get_mut("mcp_servers")
        .and_then(toml::Value::as_table_mut)
    {
        for server in servers
            .iter_mut()
            .filter_map(|(_, value)| value.as_table_mut())
        {
            server
                .entry("desktop_instance_id")
                .or_insert_with(|| "legacy".into());
        }
    }
    let dismissed = load_dismissed_ids();
    let remote = cached_remote_campaigns();
    let mut effective = layers.effective_config_base();
    let active = resolve_active_campaigns_from_layers(&layers, &effective, &remote, &dismissed);
    layers.apply_campaign_overrides(&mut effective, &active);
    Ok(effective)
}

/// Effective config with **disk campaigns only** — no remote cache, no
/// `ECHO_AGENT_CAMPAIGNS_OVERRIDE`. For one-shot CLI entrypoints that never fetch
/// remote settings: calling [`load_effective_config`] there would silently
/// resolve against a never-seeded cache, so the divergence is named instead
/// of implied (mirrors `ConfigLayers::effective_config_disk_only`).
pub fn load_effective_config_disk_only() -> std::io::Result<toml::Value> {
    Ok(ConfigLayers::load()?.effective_config_disk_only())
}

/// The effective `models.default` while an **active** campaign drives it, plus
/// the pre-campaign base value it overrode.
pub struct CampaignModelsDefault {
    /// The campaign-nudged default model.
    pub value: String,
    /// The pre-campaign base `models.default` (`None` when the user had none).
    pub pre_campaign: Option<String>,
}

/// Resolve [`CampaignModelsDefault`] fresh from the config layers, the remote
/// campaign cache, and the on-disk dismiss state.
///
/// `None` unless an active (non-dismissed, kill-switch-respecting,
/// requirements-losing) campaign changes the effective `models.default`.
/// Session creation uses this to apply a campaign to `/new` even when remote
/// settings arrived only after boot: the `ModelsManager`'s `current_model_id`
/// was resolved pre-campaign, and a campaign-only flip deliberately never
/// re-targets it (see `ModelsManager::apply_config`), so `/new` re-evaluates
/// here instead.
///
/// Reading the dismiss state fresh makes a `/model` pick win instantly:
/// [`persist_user_choice`] records the dismissal before the config write, so
/// the very next `/new` resolves campaign-free.
pub fn campaign_driven_models_default() -> Option<CampaignModelsDefault> {
    let layers = ConfigLayers::load().ok()?;
    campaign_driven_models_default_from(&layers, &cached_remote_campaigns(), &load_dismissed_ids())
}

/// Env-free resolution core of [`campaign_driven_models_default`] (unit-testable
/// without touching `ECHO_AGENT_HOME` / the process-global cache).
fn campaign_driven_models_default_from(
    layers: &ConfigLayers,
    remote: &[CampaignEntry],
    dismissed: &HashSet<String>,
) -> Option<CampaignModelsDefault> {
    let base = layers.effective_config_base();
    let active = resolve_active_campaigns_from_layers(layers, &base, remote, dismissed);
    if active.is_empty() {
        return None;
    }
    let mut effective = base.clone();
    layers.apply_campaign_overrides(&mut effective, &active);
    let base_value = read_path(&base, MODELS_DEFAULT_PATH);
    let value = read_path(&effective, MODELS_DEFAULT_PATH);
    if value == base_value {
        return None;
    }
    Some(CampaignModelsDefault {
        value: as_string(value)?,
        pre_campaign: as_string(base_value),
    })
}

/// Read the value at `path` from an effective-config tree.
fn read_path(tree: &toml::Value, path: PatchPath) -> Option<toml::Value> {
    let mut cur = tree;
    for key in path {
        cur = cur.get(*key)?;
    }
    Some(cur.clone())
}

fn as_string(v: Option<toml::Value>) -> Option<String> {
    v.and_then(|v| v.as_str().map(str::to_owned))
}

/// Resolved campaign state for one [`CampaignField`] after the overlay.
struct CampaignFieldValue {
    /// Effective value (campaign value if it won, else the merged base value).
    value: Option<toml::Value>,
    /// Whether an active campaign actually changed the effective value.
    driven: bool,
    /// Pre-campaign value to recover to; `Some` only when `driven` and the base had one.
    recovery: Option<toml::Value>,
}

/// A config field a campaign may temporarily override until the user sets it.
/// `apply_campaign_fields` drives every [`CAMPAIGN_FIELDS`] entry, so the resolve
/// pass is one row here. A field still needs its runtime state, a `persist_*`
/// writer through [`persist_user_choice`], and any field-specific reaction (e.g.
/// the model catalog-miss/live-session handling in `agent::models`).
struct CampaignField {
    /// Path into the effective config; also the dismiss key shared with the writer.
    path: PatchPath,
    /// Store the resolved value, flag, and recovery onto the agent config.
    store: fn(&mut crate::agent::config::Config, CampaignFieldValue),
    /// Clear the campaign-driven flag + recovery (value untouched). Used when
    /// resolution fails so the runtime state is defined (fail closed, matching
    /// the apply path) instead of stale.
    reset: fn(&mut crate::agent::config::Config),
}

/// Path of the `models.default` campaign field, shared by the registry row and
/// its dismiss writer so the two can't drift.
const MODELS_DEFAULT_PATH: PatchPath = &["models", "default"];

const CAMPAIGN_FIELDS: &[CampaignField] = &[CampaignField {
    path: MODELS_DEFAULT_PATH,
    store: |cfg, r| {
        cfg.models.default = as_string(r.value);
        cfg.models.default_is_campaign_driven = r.driven;
        cfg.models.pre_campaign_default = as_string(r.recovery);
    },
    reset: |cfg| {
        cfg.models.default_is_campaign_driven = false;
        cfg.models.pre_campaign_default = None;
    },
}];

/// Resolve each [`CAMPAIGN_FIELDS`] entry's value, campaign-driven flag, and
/// recovery value from the campaign overlay and store them onto `cfg`. Pure given
/// the resolved `base`/`effective`/`active`; the I/O lives in [`sync_campaign_fields`].
fn apply_campaign_fields(
    cfg: &mut crate::agent::config::Config,
    base: &toml::Value,
    effective: &toml::Value,
    active: &[CampaignEntry],
) {
    for field in CAMPAIGN_FIELDS {
        let value = read_path(effective, field.path);
        let base_value = read_path(base, field.path);
        // A campaign only *drives* a field when it actually changed the effective
        // value: requirements are re-merged after campaigns, so an admin pin wins
        // and the campaign patch is a no-op (don't flag it).
        let driven = value != base_value
            && active
                .iter()
                .any(|e| patch_touches_any(&e.patch, &[field.path]));
        let recovery = if driven { base_value } else { None };
        (field.store)(
            cfg,
            CampaignFieldValue {
                value,
                driven,
                recovery,
            },
        );
    }
}

/// Seed the remote cache, set every [`CAMPAIGN_FIELDS`] entry (value + flag +
/// recovery) from the campaign overlay, then re-apply requirements so admin pins win.
pub fn sync_campaign_fields(cfg: &mut crate::agent::config::Config) {
    let remote = remote_campaigns_from_settings(cfg.remote_settings.as_ref());
    // Seed the process-global cache from the parse we already did (skip on `None`
    // so a failed fetch can't clobber a previously-seeded cache).
    if cfg.remote_settings.is_some() {
        set_remote_campaigns(remote.clone());
    }
    let Ok(layers) = ConfigLayers::load() else {
        // Fail closed like the apply path: leave the field values as loaded but
        // clear the campaign-driven flags/recovery so they can't go stale (a
        // stale flag would mislabel a user value as campaign-driven, or vice
        // versa disarm the live-session guard for a campaign value).
        tracing::warn!("campaigns: config layer load failed; clearing campaign-driven field state");
        for field in CAMPAIGN_FIELDS {
            (field.reset)(cfg);
        }
        return;
    };
    let dismissed = load_dismissed_ids();
    let base = layers.effective_config_base();
    let active = resolve_active_campaigns_from_layers(&layers, &base, &remote, &dismissed);
    let mut effective = base.clone();
    layers.apply_campaign_overrides(&mut effective, &active);
    apply_campaign_fields(cfg, &base, &effective, &active);
    let _ = crate::config::apply_requirements(cfg);
}

/// Dismiss any active campaign whose patch touches `path`, then persist the
/// setting via `update_config`. The single field-keyed chokepoint, so a new
/// campaign-governable field is one call here with no per-field dismiss wiring.
///
/// Dismiss is recorded **before** the config write so a crash between the two
/// can't leave the campaign active over the user's just-saved value (re-nudge).
/// A dismiss-then-failed-write leaves the dismiss standing (fail-toward-no-nudge).
pub(super) async fn persist_user_choice(
    path: PatchPath,
    write: impl FnOnce(&mut super::mcp::Config),
) -> anyhow::Result<()> {
    // Config-layer reads + the flock'd read-modify-write are blocking I/O;
    // keep them off the async worker. Awaited before the config write so the
    // dismiss-before-write ordering above holds. A panicked/cancelled dismiss
    // task must NOT abort the user's write: bookkeeping failure is logged and
    // the write proceeds (the campaign may re-nudge; the pick is never lost).
    let dismissed = tokio::task::spawn_blocking(move || {
        let ids = ids_touching_paths(&resolve_dismissable_campaigns(), &[path]);
        if !ids.is_empty() {
            tracing::info!(
                ?ids,
                ?path,
                "campaigns: dismissed after the user set the field"
            );
            dismiss_campaign_ids(ids);
        }
    })
    .await;
    if let Err(e) = dismissed {
        tracing::warn!(error = %e, "campaigns: dismiss bookkeeping task failed; persisting the choice anyway");
    }
    super::persist::update_config(write).await
}

/// Persist the default model (+ optional reasoning effort) through
/// [`persist_user_choice`], so picking a model dismisses a campaign nudging
/// `models.default`. `None` clears the field.
pub async fn persist_models_default(
    value: Option<String>,
    reasoning_effort: Option<echo_agent_sampling_types::ReasoningEffort>,
) -> anyhow::Result<()> {
    let s = value.unwrap_or_default();
    if s.len() > super::settings_writes::MAX_DEFAULT_MODEL_LEN {
        anyhow::bail!(
            "model name too long ({} > {} bytes)",
            s.len(),
            super::settings_writes::MAX_DEFAULT_MODEL_LEN
        );
    }
    persist_user_choice(MODELS_DEFAULT_PATH, move |cfg| {
        cfg.models.default = if s.is_empty() { None } else { Some(s) };
        if let Some(effort) = reasoning_effort {
            cfg.models.default_reasoning_effort = Some(effort);
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_config::ConfigLayers;
    use echo_agent_test_support::EnvGuard;
    use serial_test::serial;
    use tempfile::tempdir;

    fn models_default_patch(default: &str) -> toml::Table {
        let mut models = toml::map::Map::new();
        models.insert("default".into(), toml::Value::String(default.into()));
        let mut t = toml::map::Map::new();
        t.insert("models".into(), toml::Value::Table(models));
        t
    }

    /// `ECHO_AGENT_CAMPAIGNS_OVERRIDE` applies despite the kill switch; without it the
    /// kill switch (`features.campaigns = false`) wins.
    #[test]
    #[serial]
    fn override_beats_kill_switch() {
        let base: toml::Value = toml::from_str("[features]\ncampaigns = false\n").unwrap();
        let layers = ConfigLayers::default();

        {
            let _env = EnvGuard::set(
                "ECHO_AGENT_CAMPAIGNS_OVERRIDE",
                r#"[{"id":"c","models":{"default":"m"}}]"#,
            );
            let active = resolve_active_campaigns_from_layers(&layers, &base, &[], &HashSet::new());
            assert_eq!(active.len(), 1, "override must apply despite kill switch");
            assert_eq!(active[0].id, "c");
            assert_eq!(active[0].patch["models"]["default"].as_str(), Some("m"));
        }

        // Same disabled base, override now unset → kill switch suppresses all.
        let _env = EnvGuard::unset("ECHO_AGENT_CAMPAIGNS_OVERRIDE");
        let active = resolve_active_campaigns_from_layers(&layers, &base, &[], &HashSet::new());
        assert!(
            active.is_empty(),
            "kill switch wins when override is absent"
        );
    }

    /// Invalid `ECHO_AGENT_CAMPAIGNS_OVERRIDE` JSON fails toward *no campaigns*: the
    /// var's intent is "replace campaigns with exactly this", so a typo must not
    /// silently re-enable the layer/remote campaigns it was meant to replace.
    #[test]
    #[serial]
    fn invalid_override_json_suppresses_all_campaigns() {
        let _env = EnvGuard::set("ECHO_AGENT_CAMPAIGNS_OVERRIDE", "{ not json");

        let mut layers = ConfigLayers::default();
        layers.campaigns.user = vec![CampaignEntry {
            id: "from-layer".into(),
            patch: models_default_patch("layer-model"),
        }];
        let remote = vec![CampaignEntry {
            id: "from-remote".into(),
            patch: models_default_patch("remote-model"),
        }];
        let base = toml::Value::Table(Default::default());

        let active = resolve_active_campaigns_from_layers(&layers, &base, &remote, &HashSet::new());
        assert!(
            active.is_empty(),
            "an invalid override must suppress all campaigns, not fall back to real sources"
        );
    }

    /// Dismiss bookkeeping deliberately ignores the kill switch: a model pick
    /// made while `ECHO_AGENT_CAMPAIGNS=0` must still record the dismissal, or a
    /// later re-enabled campaign would override the user's explicit choice.
    #[test]
    #[serial]
    fn dismiss_resolution_ignores_kill_switch() {
        let _over = EnvGuard::unset("ECHO_AGENT_CAMPAIGNS_OVERRIDE");
        let _kill = EnvGuard::set("ECHO_AGENT_CAMPAIGNS", "0");

        let mut patch = serde_json::Map::new();
        patch.insert("models".into(), serde_json::json!({ "default": "m" }));
        let rs = RemoteSettings {
            campaigns: vec![CampaignOverride {
                id: Some("dismiss-during-kill-switch".into()),
                patch,
            }],
            ..Default::default()
        };
        set_remote_campaigns_from_settings(Some(&rs));

        let resolved = resolve_dismissable_campaigns();
        // Clear the process-global cache before asserting so a failure can't
        // leak state into sibling tests.
        set_remote_campaigns_from_settings(Some(&RemoteSettings::default()));
        assert!(
            resolved
                .iter()
                .any(|c| c.id == "dismiss-during-kill-switch"),
            "kill switch must not hide a campaign from dismiss bookkeeping"
        );
    }

    /// `campaign_driven_models_default_from` tracks remote entries and
    /// dismissals: `Some` while the campaign is active, `None` the instant its
    /// dismissal lands, so a `/new` right after a `/model` pick never re-nudges.
    #[test]
    #[serial]
    fn campaign_driven_models_default_tracks_remote_and_dismissals() {
        let _over = EnvGuard::unset("ECHO_AGENT_CAMPAIGNS_OVERRIDE");
        let _kill = EnvGuard::unset("ECHO_AGENT_CAMPAIGNS");

        let layers = ConfigLayers {
            user: toml::from_str("[models]\ndefault = \"config-model\"\n").unwrap(),
            ..Default::default()
        };
        let remote = vec![CampaignEntry {
            id: "t-models-nudge".into(),
            patch: models_default_patch("campaign-model"),
        }];

        let nudge = campaign_driven_models_default_from(&layers, &remote, &HashSet::new())
            .expect("active campaign drives the default");
        assert_eq!(nudge.value, "campaign-model");
        assert_eq!(nudge.pre_campaign.as_deref(), Some("config-model"));

        // A dismissal (what a `/model` pick records first) deactivates the
        // nudge for the very next resolution.
        let dismissed: HashSet<String> = ["t-models-nudge".to_string()].into_iter().collect();
        assert!(
            campaign_driven_models_default_from(&layers, &remote, &dismissed).is_none(),
            "a dismissed campaign must not nudge"
        );

        // A campaign that loses to a requirements pin never reports
        // campaign-driven.
        let mut pinned = layers.clone();
        pinned.user_requirements =
            Some(toml::from_str("[models]\ndefault = \"config-model\"\n").unwrap());
        assert!(
            campaign_driven_models_default_from(&pinned, &remote, &HashSet::new()).is_none(),
            "a requirements-pinned default must not report campaign-driven"
        );
    }

    /// `ECHO_AGENT_CAMPAIGNS_OVERRIDE="[]"` replaces all sources with nothing — even
    /// layer + remote campaigns resolve to empty.
    #[test]
    #[serial]
    fn override_empty_means_none() {
        let _env = EnvGuard::set("ECHO_AGENT_CAMPAIGNS_OVERRIDE", "[]");

        let mut layers = ConfigLayers::default();
        layers.campaigns.user = vec![CampaignEntry {
            id: "from-layer".into(),
            patch: models_default_patch("layer-model"),
        }];
        let remote = vec![CampaignEntry {
            id: "from-remote".into(),
            patch: models_default_patch("remote-model"),
        }];
        let base = toml::Value::Table(Default::default());

        let active = resolve_active_campaigns_from_layers(&layers, &base, &remote, &HashSet::new());
        assert!(
            active.is_empty(),
            "empty override replaces all sources, yielding no campaigns"
        );
    }

    /// Contract: `persist_user_choice(["models","default"], ..)` dismisses only
    /// campaigns that touch that path, never a sibling-field campaign. The full
    /// wiring (set_default_model -> persist -> dismiss) is covered end to end by
    /// the pager `pty_e2e` campaign test.
    #[test]
    fn models_default_persist_targets_only_model_campaigns() {
        let model_campaign = CampaignEntry {
            id: "release".into(),
            patch: models_default_patch("new-model"),
        };
        let other_campaign = CampaignEntry {
            id: "other".into(),
            patch: toml::from_str::<toml::Table>("[features]\nweb_fetch = true\n").unwrap(),
        };
        let path: &[PatchPath] = &[&["models", "default"]];
        let ids = ids_touching_paths(&[model_campaign, other_campaign], path);
        assert_eq!(ids, vec!["release".to_string()]);
    }

    /// `apply_campaign_fields` flags a field campaign-driven only when the campaign
    /// actually changed the effective value: a campaign win sets the flag + recovery,
    /// but a requirements win (effective == base) does not (and stores no recovery).
    #[test]
    fn campaign_field_flags_campaign_win_not_requirements_win() {
        let active = vec![CampaignEntry {
            id: "release".into(),
            patch: models_default_patch("campaign-model"),
        }];
        let base: toml::Value = toml::from_str("[models]\ndefault = \"base-model\"\n").unwrap();

        // Campaign won the effective default.
        let mut cfg = crate::agent::config::Config::default();
        let won: toml::Value = toml::from_str("[models]\ndefault = \"campaign-model\"\n").unwrap();
        apply_campaign_fields(&mut cfg, &base, &won, &active);
        assert_eq!(cfg.models.default.as_deref(), Some("campaign-model"));
        assert!(cfg.models.default_is_campaign_driven);
        assert_eq!(
            cfg.models.pre_campaign_default.as_deref(),
            Some("base-model")
        );

        // Requirements re-merge clobbered the campaign back to the base value.
        let mut cfg = crate::agent::config::Config::default();
        apply_campaign_fields(&mut cfg, &base, &base, &active);
        assert_eq!(cfg.models.default.as_deref(), Some("base-model"));
        assert!(!cfg.models.default_is_campaign_driven);
        assert_eq!(cfg.models.pre_campaign_default, None);

        // No active campaign touching the field: never driven.
        let mut cfg = crate::agent::config::Config::default();
        apply_campaign_fields(&mut cfg, &base, &won, &[]);
        assert!(!cfg.models.default_is_campaign_driven);
        assert_eq!(cfg.models.pre_campaign_default, None);
    }

    /// Fix: in leader mode the pager seeds the remote-campaign cache so the
    /// dismiss path (which runs in the pager process) can see remote campaigns.
    /// Verify a seeded remote campaign round-trips into the dismiss-id set.
    #[test]
    #[serial]
    fn seeded_remote_campaign_is_visible_to_dismiss() {
        let _env = EnvGuard::unset("ECHO_AGENT_CAMPAIGNS_OVERRIDE");
        let mut patch = serde_json::Map::new();
        patch.insert("models".into(), serde_json::json!({ "default": "m" }));
        let rs = RemoteSettings {
            campaigns: vec![CampaignOverride {
                id: Some("remote-1".into()),
                patch,
            }],
            ..Default::default()
        };
        set_remote_campaigns_from_settings(Some(&rs));
        let cached = cached_remote_campaigns();
        assert!(cached.iter().any(|c| c.id == "remote-1"));

        let path: &[PatchPath] = &[&["models", "default"]];
        assert_eq!(
            ids_touching_paths(&cached, path),
            vec!["remote-1".to_string()]
        );

        // Clear the process-global cache so other tests aren't affected.
        set_remote_campaigns_from_settings(Some(&RemoteSettings::default()));
    }

    /// An override-supplied campaign whose id is already dismissed is dropped.
    #[test]
    #[serial]
    fn dismissed_id_is_dropped_from_override() {
        let _env = EnvGuard::set(
            "ECHO_AGENT_CAMPAIGNS_OVERRIDE",
            r#"[{"id":"seen","models":{"default":"m"}}]"#,
        );
        let layers = ConfigLayers::default();
        let base = toml::Value::Table(Default::default());
        let dismissed: HashSet<String> = ["seen".to_owned()].into_iter().collect();
        let active = resolve_active_campaigns_from_layers(&layers, &base, &[], &dismissed);
        assert!(active.is_empty(), "a dismissed id must not re-apply");
    }

    /// Corrupt `campaigns_state.json` is preserved as `*.json.corrupt`, the new
    /// dismiss still lands, and the cap drops the oldest ids.
    #[test]
    fn dismiss_persists_handles_corrupt_and_caps() {
        let home = tempdir().unwrap();
        std::fs::write(campaigns_state_path(home.path()), "{ not json").unwrap();
        dismiss_campaign_ids_at(home.path(), ["new-id".to_owned()]).unwrap();
        assert!(
            home.path().join("campaigns_state.json.corrupt").exists(),
            "corrupt state must be renamed aside, not discarded"
        );

        dismiss_campaign_ids_at(home.path(), (0..40).map(|i| format!("id-{i}"))).unwrap();
        let contents = std::fs::read_to_string(campaigns_state_path(home.path())).unwrap();
        let set: HashSet<String> = serde_json::from_str::<CampaignsState>(&contents)
            .unwrap()
            .dismissed_ids
            .into_iter()
            .collect();
        assert_eq!(set.len(), MAX_DISMISSED_IDS);
        assert!(set.contains("id-39"));
        assert!(!set.contains("new-id"), "oldest ids evicted past the cap");
    }

    /// A remote campaign's flattened JSON patch becomes a full TOML patch (any
    /// field), and an id-less entry is dropped.
    #[test]
    fn remote_campaign_to_entry_builds_full_patch() {
        let mut patch = serde_json::Map::new();
        patch.insert(
            "models".into(),
            serde_json::json!({ "default": "remote-model" }),
        );
        patch.insert("features".into(), serde_json::json!({ "web_fetch": true }));
        let entry = remote_campaign_to_entry(CampaignOverride {
            id: Some("r1".into()),
            patch,
        })
        .expect("entry with id + patch survives");
        assert_eq!(
            entry.patch["models"]["default"].as_str(),
            Some("remote-model")
        );
        assert_eq!(entry.patch["features"]["web_fetch"].as_bool(), Some(true));

        let no_id = CampaignOverride {
            id: None,
            patch: {
                let mut p = serde_json::Map::new();
                p.insert("models".into(), serde_json::json!({ "default": "x" }));
                p
            },
        };
        assert!(remote_campaign_to_entry(no_id).is_none());
    }

    /// The remote JSON shape accepts `campaign_id` as an alias for `id`, matching
    /// the TOML `CampaignMeta` contract so the two sides can't drift. The id key
    /// (either spelling) must be *consumed*, never leak into the flattened patch —
    /// a leaked key would deep-merge junk into every effective config.
    #[test]
    fn campaign_id_json_alias_is_accepted_and_does_not_leak_into_patch() {
        for raw in [
            r#"[{"campaign_id":"r1","models":{"default":"m"}}]"#,
            r#"[{"id":"r1","models":{"default":"m"}}]"#,
        ] {
            let list: Vec<CampaignOverride> = serde_json::from_str(raw).unwrap();
            let entry = remote_campaign_to_entry(list.into_iter().next().unwrap())
                .expect("entry with id survives");
            assert_eq!(entry.id, "r1");
            assert!(
                entry.patch.get("id").is_none() && entry.patch.get("campaign_id").is_none(),
                "id keys must not leak into the patch: {raw}"
            );
        }
    }

    /// Every registry row's `reset` clears the campaign-driven runtime state a
    /// prior `store` set (used on the resolution-failure path so flags can't go
    /// stale).
    #[test]
    fn campaign_field_reset_clears_driven_state() {
        let mut cfg = crate::agent::config::Config::default();
        for field in CAMPAIGN_FIELDS {
            (field.store)(
                &mut cfg,
                CampaignFieldValue {
                    value: Some(toml::Value::String("campaign-model".into())),
                    driven: true,
                    recovery: Some(toml::Value::String("base-model".into())),
                },
            );
        }
        assert!(cfg.models.default_is_campaign_driven);
        assert!(cfg.models.pre_campaign_default.is_some());

        for field in CAMPAIGN_FIELDS {
            (field.reset)(&mut cfg);
        }
        assert!(!cfg.models.default_is_campaign_driven);
        assert_eq!(cfg.models.pre_campaign_default, None);
        // The field *value* is left as loaded; reset only clears the metadata.
        assert_eq!(cfg.models.default.as_deref(), Some("campaign-model"));
    }
}
