//! Config file loading for EchoAgent.
//!
//! Merge order (lowest → highest priority):
//! 1. `/etc/echoagent/managed_config.toml`
//! 2. `$ECHO_AGENT_HOME/managed_config.toml`
//! 3. `$ECHO_AGENT_HOME/config.toml`
//! 4. `$ECHO_AGENT_HOME/requirements.toml` (cloud cache; Ed25519-signed at rest once a
//!    key is embedded — see [`signed_policy`] — below the OS-protected layers)
//! 5. `/etc/echoagent/requirements.toml`
//! 6. macOS MDM managed preferences (`ai.x.echo_agent`, admin-forced) — macOS only
//!
//! Each layer applies its own [`[[version_overrides]]`](version_overrides)
//! before merge. Requirements layers (#4–#6) may opt into fail-closed startup;
//! see [`validate_requirements`].

pub mod campaigns;
mod config_layers;
pub mod config_override;
mod env_overlay;
pub mod fs_atomic;
pub mod global_hook_sources;
mod loader;
mod macos_managed;
mod managed_cache;
pub mod managed_text;
mod paths;
pub mod shell;
pub mod signed_policy;
mod validation;
pub mod version_overrides;

// Only the cross-crate campaign surface is re-exported at the root; the rest stays
// reachable via the `pub mod` paths for in-crate use without widening the API.
pub use campaigns::{
    CampaignEntry, CampaignOverrides, filter_active_campaigns, ids_touching_paths,
};
pub use global_hook_sources::{
    GlobalHookSource, GlobalHookSourceError, GlobalHookSourceKind, ResolvedGlobalHookSources,
    ensure_echo_agent_hook_slots, existing_ancestor_chain, is_direct_hook_json_name,
    list_direct_hook_json_files, missing_configured_sources, path_has_symlink_component,
    resolve_global_hook_sources, unique_ancestors_rootward,
};

pub use config_layers::{
    CampaignsState, ConfigLayers, campaigns_application_disabled, campaigns_state_path,
    load_dismissed_ids_from_home, load_effective_config_disk_only,
};
pub use env_overlay::{
    ECHO_AGENT_CONFIG_ENV, ECHO_AGENT_CONFIG_PATH_ENV, OverlaySource, ResolvedOverlay,
    resolved_env_overlay,
};
#[cfg(unix)]
pub use global_hook_sources::{
    validate_direct_hook_json_file, validated_hook_json_files_for_sources,
};
pub use loader::{
    HookConfigLayer, HookProvenance, MANAGED_CONFIG_FILENAME, ManagedConfigLayer,
    REQUIREMENTS_FILENAME, USER_CONFIG_FILENAME, apply_version_overrides_with_registered,
    deep_merge_toml, expand_env_vars_in_string, expand_env_vars_in_toml, hook_config_layers,
    hook_config_layers_at, load_config_file, load_from_disk, load_managed_config,
    load_system_managed_config, load_toml_file, managed_config_layers, managed_config_layers_at,
    toml_error_detail,
};
pub use macos_managed::MDM_REQUIREMENTS_SOURCE;
pub use managed_cache::{
    MANAGED_CONFIG_CACHE_FILE, ServingIdentity, SyncMarker, bump_rollback_floor,
    bump_rollback_floor_with_now, confirmed_team_switch, confirmed_team_switch_at,
    fail_closed_policy_armed_at, is_managed_config_hard_stale_for, is_managed_config_stale_for,
    managed_config_identity_changed_at, managed_deployment_id, managed_policy_compromised_for,
    mark_managed_config_synced, mark_managed_config_synced_at, normalize_identity,
};
pub use paths::{
    claude_managed_settings_path, claude_managed_settings_probe_path, create_dir_all_owner_only,
    decode_cwd_from_dirname, default_echo_agent_home, echo_agent_application,
    echo_agent_application_in, echo_agent_home, encode_cwd_dirname, ensure_sessions_cwd_dir,
    ensure_sessions_cwd_dir_in, sessions_cwd_dir, sessions_cwd_dir_in, set_dir_owner_only,
    system_config_dir, user_echo_agent_home,
};
pub use validation::{
    RequirementsError, RequirementsLayer, RequirementsSource, load_merged_requirements,
    requirements_layers, validate_requirements,
};
pub use version_overrides::{VersionOverrideError, apply_version_overrides};

/// Parse an env var as a boolean. `None` if unset or unrecognized.
pub fn env_bool(name: &str) -> Option<bool> {
    let value = std::env::var(name).ok()?;
    match value.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "1" | "true" | "yes" | "on" | "enabled" => Some(true),
        "0" | "false" | "no" | "off" | "disabled" => Some(false),
        _ => None,
    }
}

pub use paths::{controlled_discovery_home, discovery_home_dir, resolve_discovery_home_with};
