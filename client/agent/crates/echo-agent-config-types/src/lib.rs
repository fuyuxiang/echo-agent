#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
mod flags;
pub use flags::*;
mod registry;
pub use registry::*;
mod memory;
pub use memory::*;
mod mcp;
pub use mcp::*;
mod permission;
pub use permission::*;
mod pool;
use echo_agent_announcements::RemoteAnnouncement;
pub use pool::*;
use serde::{Deserialize, Serialize};
/// A remote `campaigns[]` entry: an `id` gate plus a full-power
/// flattened config patch (the JSON sibling of a `[[campaigns]]` TOML override).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CampaignOverride {
    #[serde(default, alias = "campaign_id")]
    pub id: Option<String>,
    #[serde(flatten, default)]
    pub patch: serde_json::Map<String, serde_json::Value>,
}
/// Doom-loop recovery settings: ONE struct serves both the local
/// `[doom_loop_recovery]` TOML table and the remote settings
/// `doom_loop_recovery` JSON object, so the two stay 1:1. All fields are
/// `Option` with per-field defaults (a partial object never fails the parse,
/// and unknown future keys are ignored); unset fields fall through per-field
/// in `resolve_doom_loop_recovery` (env > TOML > remote > default). Distinct
/// namespace from the removed legacy `doom_loop_*` keys.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DoomLoopRecoverySettings {
    /// Send the `x-echo-agent-doom-loop-check` header, parse the reported
    /// triggers, and resample confident loops. `Some(false)` is a
    /// kill-switch; absent ⇒ client default (ON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Highest `tail_repetition` threshold considered confident (clamped to
    /// 2..=64). Absent ⇒ client default (64). CLIENT-side filter over the
    /// trigger labels the server returns — the server emits every fired
    /// threshold; this is never sent as a request parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_threshold: Option<u32>,
    /// Resample budget per turn (clamped to 0..=5). Absent ⇒ client
    /// default (2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// Detector window sent as the value of `x-echo-agent-doom-loop-check`
    /// (honored in 512..=4096, otherwise 4096; absent ⇒ client default 1024).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_tokens: Option<u32>,
}
/// Per-kind age policy for auto-GC: seconds or never.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeKindMaxAge {
    Secs(u64),
    Never,
}
impl Serialize for WorktreeKindMaxAge {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Secs(n) => serializer.serialize_u64(*n),
            Self::Never => serializer.serialize_str("never"),
        }
    }
}
impl<'de> Deserialize<'de> for WorktreeKindMaxAge {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = WorktreeKindMaxAge;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("u64 seconds or \"never\"")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(WorktreeKindMaxAge::Secs(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                u64::try_from(v)
                    .map(WorktreeKindMaxAge::Secs)
                    .map_err(E::custom)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                if v.eq_ignore_ascii_case("never") {
                    Ok(WorktreeKindMaxAge::Never)
                } else if let Ok(n) = v.parse::<u64>() {
                    Ok(WorktreeKindMaxAge::Secs(n))
                } else {
                    Err(E::custom("expected \"never\" or integer seconds"))
                }
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(WorktreeKindMaxAge::Never)
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(WorktreeKindMaxAge::Never)
            }
        }
        deserializer.deserialize_any(V)
    }
}
/// Local `[worktree.auto_gc]` / remote `worktree_auto_gc` policy.
/// Field-wise tolerant deserialize so one bad key cannot drop a sibling kill-switch.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct WorktreeAutoGcSettings {
    /// `Some(false)` kill-switch; absent ⇒ default on (env kill still applies).
    #[serde(
        default,
        deserialize_with = "de_opt_bool_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub enabled: Option<bool>,
    /// Age cutoff seconds when platform age-expiry is allowed; clamped by resolver.
    #[serde(
        default,
        deserialize_with = "de_opt_u64_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_age_secs: Option<u64>,
    /// Min seconds between successful auto-GC stamps; clamped by resolver.
    #[serde(
        default,
        deserialize_with = "de_opt_u64_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_interval_secs: Option<u64>,
    /// Count age candidates without deleting.
    #[serde(
        default,
        deserialize_with = "de_opt_bool_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub dry_run: Option<bool>,
    /// Linux only.
    #[serde(
        default,
        deserialize_with = "de_opt_bool_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub include_orphan_snapshots: Option<bool>,
    /// Per-kind max ages (`session`/`ab`/`pool`/`fork`/`manual`/`subagent`).
    /// Seconds or `"never"`. Absent keys use defaults (client default: `manual`=never).
    /// Remote may set `manual` to a finite TTL — not client-pinned; local TOML can restore `"never"`.
    /// Unknown kind keys ignored at resolve.
    #[serde(
        default,
        deserialize_with = "de_opt_max_age_by_kind_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_age_by_kind: Option<std::collections::BTreeMap<String, WorktreeKindMaxAge>>,
    /// Optional discovery rebuild + echo-agent-scoped stale `.git/worktrees/` scrub (default off).
    #[serde(
        default,
        deserialize_with = "de_opt_bool_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub include_rebuild: Option<bool>,
    /// Independent rebuild throttle seconds; absent ⇒ 24h. Clamped like `min_interval_secs`.
    #[serde(
        default,
        deserialize_with = "de_opt_u64_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub rebuild_min_interval_secs: Option<u64>,
}
/// Display-refresh probe + auto-cadence settings: ONE struct for local
/// `[ui.display_refresh]`, remote settings `display_refresh`, and `UiConfig`.
/// Field-wise tolerant deserialize (wrong types → `None`); unknown keys kept in
/// [`Self::extra`] so settings save cannot drop future knobs. Resolved by
/// `resolve_display_refresh`. Client defaults: probe on, auto on, floor 8 ms,
/// ceiling 16 ms, Hz band 55–240.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DisplayRefreshSettings {
    /// Once-per-process primary-display Hz probe. `Some(false)` is a kill-switch.
    #[serde(
        default,
        deserialize_with = "de_opt_bool_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub probe_enabled: Option<bool>,
    /// Derive paint/scroll cadence from a successful in-band probe (default off).
    #[serde(
        default,
        deserialize_with = "de_opt_bool_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub auto_cadence_enabled: Option<bool>,
    /// Lower clamp for auto-derived ms (default 8).
    #[serde(
        default,
        deserialize_with = "de_opt_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub floor_ms: Option<u32>,
    /// Upper clamp for auto-derived ms (default 16).
    #[serde(
        default,
        deserialize_with = "de_opt_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub ceiling_ms: Option<u32>,
    /// Minimum accepted probe Hz for auto-cadence (default 55).
    #[serde(
        default,
        deserialize_with = "de_opt_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub min_hz: Option<u32>,
    /// Maximum accepted probe Hz for auto-cadence (default 165).
    #[serde(
        default,
        deserialize_with = "de_opt_u32_tolerant",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_hz: Option<u32>,
    /// Unknown / future object members (preserved across config rewrite).
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
impl DisplayRefreshSettings {
    /// True when no field is set (all inherit remote/default).
    pub fn is_default(&self) -> bool {
        self.probe_enabled.is_none()
            && self.auto_cadence_enabled.is_none()
            && self.floor_ms.is_none()
            && self.ceiling_ms.is_none()
            && self.min_hz.is_none()
            && self.max_hz.is_none()
            && self.extra.is_empty()
    }
}
fn de_opt_bool_tolerant<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<bool>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bool (wrong types ignored)")
        }
        fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<A: serde::de::Deserializer<'de>>(
            self,
            d: A,
        ) -> Result<Self::Value, A::Error> {
            d.deserialize_any(V)
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(V)
}
fn de_opt_u32_tolerant<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u32>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<u32>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("u32 (wrong types ignored)")
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(u32::try_from(v).ok())
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(u32::try_from(v).ok())
        }
        fn visit_u32<E: serde::de::Error>(self, v: u32) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i32<E: serde::de::Error>(self, v: i32) -> Result<Self::Value, E> {
            Ok(u32::try_from(v).ok())
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<A: serde::de::Deserializer<'de>>(
            self,
            d: A,
        ) -> Result<Self::Value, A::Error> {
            d.deserialize_any(V)
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(V)
}
fn de_opt_u64_tolerant<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Option<u64>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("u64 (wrong types ignored)")
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(u64::try_from(v).ok())
        }
        fn visit_u32<E: serde::de::Error>(self, v: u32) -> Result<Self::Value, E> {
            Ok(Some(u64::from(v)))
        }
        fn visit_i32<E: serde::de::Error>(self, v: i32) -> Result<Self::Value, E> {
            Ok(u64::try_from(v).ok())
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<A: serde::de::Deserializer<'de>>(
            self,
            d: A,
        ) -> Result<Self::Value, A::Error> {
            d.deserialize_any(V)
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
    }
    deserializer.deserialize_any(V)
}
/// Tolerant map: bad whole value → None; per-entry bad values skipped.
fn de_opt_max_age_by_kind_tolerant<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<std::collections::BTreeMap<String, WorktreeKindMaxAge>>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Object(map) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map {
                if let Ok(age) = serde_json::from_value::<WorktreeKindMaxAge>(v) {
                    out.insert(k, age);
                }
            }
            Ok(Some(out))
        }
        _ => Ok(None),
    }
}
/// Present-but-malformed nested value: `None` plus a warning, so one bad
/// setting cannot fail the whole [`RemoteSettings`] parse.
pub fn deserialize_tolerant<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let Some(value) =
        Option::<serde_json::Value>::deserialize(deserializer)?.filter(|v| !v.is_null())
    else {
        return Ok(None);
    };
    match serde_json::from_value::<T>(value) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                setting = std::any::type_name::<T>(),
                "ignoring malformed remote setting; falling through to local defaults"
            );
            Ok(None)
        }
    }
}
/// Consent notice from `echo_agent_build_settings.consent_gate`. Which accounts see it is a targeting
/// decision, not a property of the payload.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConsentGate {
    /// Keys the stored answer and the upstream record. A payload without one is dropped whole.
    pub id: String,
    /// Acceptance version for this notice. Only comparable against an answer to the same `id`.
    #[serde(default)]
    pub version: Option<i32>,
    #[serde(default)]
    pub title: Option<String>,
    /// `[label](https://…)` links are painted inline; there is no scrolling, so keep it short.
    #[serde(default)]
    pub body: Option<String>,
    /// The only way past the notice.
    #[serde(default)]
    pub accept_label: Option<String>,
}
/// Remote settings fetched from cli-chat-proxy `GET /v1/settings`.
///
/// All fields are `Option` with `#[serde(default)]` so that:
/// - Missing fields from old servers are gracefully ignored
/// - New fields added in the future don't break existing clients
/// - Callers can distinguish "server said false" from "server didn't say"
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RemoteSettings {
    /// When `Some(true)`, the server recommends enabling leader mode.
    /// Used as a fallback when the user hasn't set `[cli] use_leader` locally.
    #[serde(default)]
    pub leader_mode: Option<bool>,
    #[serde(default)]
    pub max_upload_file_bytes: Option<u64>,
    #[serde(default)]
    pub max_upload_untracked_bytes: Option<u64>,
    /// When `Some(true)`, capture workspace files for non-git project dirs (client default: off).
    #[serde(default)]
    pub non_git_workspace_capture: Option<bool>,
    #[serde(default)]
    pub login_shell_capture: Option<bool>,
    /// When `Some(false)`, scheduled task fires run as main-conversation
    /// turns instead of background subagents.
    #[serde(default)]
    pub scheduler_background_loops: Option<bool>,
    /// Release channel: `"stable"` or `"alpha"`.
    /// Fallback when no local `[cli] channel` or `--alpha`/`--stable` flag is set.
    #[serde(default)]
    pub release_channel: Option<String>,
    /// When `Some(true)`, enable LOC attribution tracking for this session.
    #[serde(default)]
    pub loc_tracking: Option<bool>,
    /// Enable the experimental memory system remotely.
    #[serde(default)]
    pub memory_enabled: Option<bool>,
    #[serde(default)]
    pub memory_search_max_results: Option<u32>,
    #[serde(default)]
    pub memory_search_min_score: Option<f32>,
    #[serde(default)]
    pub memory_initial_injection_enabled: Option<bool>,
    #[serde(default)]
    pub memory_initial_injection_min_score: Option<f32>,
    #[serde(default)]
    pub memory_embedding_model: Option<String>,
    #[serde(default)]
    pub memory_embedding_dimensions: Option<u32>,
    #[serde(default)]
    pub pruning_enabled: Option<bool>,
    #[serde(default)]
    pub pruning_keep_last_n_turns: Option<u32>,
    #[serde(default)]
    pub pruning_soft_trim_threshold: Option<u32>,
    #[serde(default)]
    pub flush_enabled: Option<bool>,
    #[serde(default)]
    pub flush_soft_threshold_tokens: Option<u64>,
    #[serde(default)]
    pub flush_idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub flush_semantic_dedup_threshold: Option<f64>,
    #[serde(default)]
    pub memory_temporal_decay_enabled: Option<bool>,
    #[serde(default)]
    pub memory_temporal_decay_half_life_days: Option<f64>,
    #[serde(default)]
    pub memory_mmr_enabled: Option<bool>,
    #[serde(default)]
    pub memory_mmr_lambda: Option<f64>,
    #[serde(default)]
    pub memory_watcher_enabled: Option<bool>,
    #[serde(default)]
    pub dream_enabled: Option<bool>,
    #[serde(default)]
    pub dream_min_hours: Option<u64>,
    #[serde(default)]
    pub dream_min_sessions: Option<u64>,
    #[serde(default)]
    pub dream_check_interval_secs: Option<u64>,
    /// Cadence (seconds) of the pager's free→paid subscription watch.
    /// `0` disables it; the pager clamps and defaults (see its
    /// `app::subscription` module). Forwarded from the `echo_agent_build_settings`
    /// remote settings flag via the CCP `/settings` flatten catch-all.
    #[serde(default)]
    pub subscription_watch_interval_secs: Option<u64>,
    #[serde(default)]
    pub writeback_enabled: Option<bool>,
    /// OAuth2 provider issuer URL (e.g., "https://auth.echo-agent.invalid"). When present
    /// together with `oauth2_client_id`, the client uses OAuth2 authorization code
    /// flow. Controlled via remote settings for gradual rollout.
    #[serde(default)]
    pub oauth2_issuer: Option<String>,
    /// OAuth2 client_id for the CLI. Paired with `oauth2_issuer`.
    #[serde(default)]
    pub oauth2_client_id: Option<String>,
    /// When `Some(true)`, enable echoagent's default OAuth2 (EchoAgent auth.echo-agent.invalid).
    /// Enterprise OIDC (user's own IdP via `oidc` config) always wins.
    /// Controlled via remote settings; `--oauth` CLI flag overrides.
    #[serde(default)]
    pub echo_agent_oauth_enabled: Option<bool>,
    #[serde(default)]
    pub lsp_tools_enabled: Option<bool>,
    /// Folder-trust gate kill-switch / remote default. Gates whether repo-local
    /// MCP/LSP servers (commands sourced from working-tree config files) require
    /// a per-folder trust decision before they are spawned. `Some(true)`
    /// enables, `Some(false)` is a kill-switch, `None` falls back to the client
    /// default (on). Sits below env `ECHO_AGENT_FOLDER_TRUST`, user
    /// `[folder_trust] enabled`, and managed config in the resolver chain. See
    /// `agent::folder_trust::feature_enabled`.
    #[serde(default)]
    pub folder_trust_enabled: Option<bool>,
    #[serde(default)]
    pub write_file_enabled: Option<bool>,
    /// File toolset: `"standard"` or `"hashline"`.
    /// Server-side default; local `[toolset] file_toolset` in config.toml
    /// takes precedence when set.
    #[serde(default)]
    pub file_toolset: Option<String>,
    /// Per-chunk idle timeout in seconds for inference streaming.
    /// Fallback when no per-model `inference_idle_timeout_secs` is set in config.toml.
    #[serde(default)]
    pub inference_idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub subagent_rate_limit_max_attempts: Option<u32>,
    /// Global default MCP startup-handshake timeout (seconds); lowest-precedence
    /// fallback (per-server config, env, and requirements/managed override it).
    #[serde(default)]
    pub mcp_startup_timeout_secs: Option<u64>,
    /// remote settings `echo_agent_build_settings.max_mcp_output_bytes` — global default
    /// MCP tool-result inline cap (bytes). Overridden by requirements, env,
    /// and `config.toml [mcp] max_output_bytes`. Built-in default 20_000.
    #[serde(default)]
    pub max_mcp_output_bytes: Option<u64>,
    /// When `Some(true)`, enable session registry hooks (register, update, finalize, memory upload).
    /// When absent or `Some(false)`, all hooks are disabled (default: disabled).
    #[serde(default)]
    pub session_registry_enabled: Option<bool>,
    /// The remote settings `doom_loop_recovery` JSON object; see
    /// [`DoomLoopRecoverySettings`]. Absent ⇒ every knob falls through to
    /// TOML/defaults; a partial object falls through per-field.
    #[serde(default)]
    pub doom_loop_recovery: Option<DoomLoopRecoverySettings>,
    /// Automatic worktree GC policy; see [`WorktreeAutoGcSettings`].
    /// Absent ⇒ every knob falls through to TOML/defaults; a partial object
    /// falls through per-field. Present-but-malformed nested value is dropped
    /// to `None` (does not fail the whole `RemoteSettings` parse). Platform
    /// age-expiry policy is client-hardcoded and not remote-overridable.
    #[serde(default, deserialize_with = "deserialize_tolerant")]
    pub worktree_auto_gc: Option<WorktreeAutoGcSettings>,
    /// Enable/disable the runtime turn-end TodoGate remotely.
    /// Precedence: CLI `--todo-gate` > this field > built-in default (`false`).
    /// The gate ships disabled; set this to `Some(true)` (via the
    /// `echo_agent_build_settings` remote settings key) to enable it. See
    /// `session::acp_session::resolve_reminder_policy`.
    #[serde(default)]
    pub todo_gate_enabled: Option<bool>,
    /// Hard cap on TodoGate fires per user prompt.
    /// Precedence: this field > built-in default (`DEFAULT_TODO_GATE_MAX_FIRES`).
    /// No CLI override. See `session::acp_session::resolve_reminder_policy`.
    #[serde(default)]
    pub todo_gate_max_fires_per_prompt: Option<u32>,
    #[serde(default)]
    pub auto_wake_enabled: Option<bool>,
    #[serde(default)]
    pub cursor_skills_enabled: Option<bool>,
    #[serde(default)]
    pub cursor_rules_enabled: Option<bool>,
    #[serde(default)]
    pub cursor_agents_enabled: Option<bool>,
    #[serde(default)]
    pub claude_skills_enabled: Option<bool>,
    #[serde(default)]
    pub claude_rules_enabled: Option<bool>,
    #[serde(default)]
    pub claude_agents_enabled: Option<bool>,
    #[serde(default)]
    pub cursor_mcps_enabled: Option<bool>,
    #[serde(default)]
    pub cursor_hooks_enabled: Option<bool>,
    #[serde(default)]
    pub claude_mcps_enabled: Option<bool>,
    #[serde(default)]
    pub claude_hooks_enabled: Option<bool>,
    #[serde(default)]
    pub cursor_sessions_enabled: Option<bool>,
    #[serde(default)]
    pub claude_sessions_enabled: Option<bool>,
    #[serde(default)]
    pub codex_sessions_enabled: Option<bool>,
    /// When `Some(true)`, enable goal mode remotely.
    /// When `Some(false)`, force-disable it (kill-switch).
    /// Absent ⇒ client default (enabled).
    #[serde(default)]
    pub goal_enabled: Option<bool>,
    /// When `Some(true)`, enable the goal-completion classifier remotely.
    /// When `Some(false)`, force-disable it.
    /// Absent ⇒ default tracks goal mode (enabled iff goal mode is on).
    #[serde(default)]
    pub goal_classifier_enabled: Option<bool>,
    /// When `Some(true)`, enable the goal planner remotely.
    /// When `Some(false)`, force-disable it.
    /// Absent ⇒ default tracks goal mode (enabled iff goal mode is on).
    #[serde(default)]
    pub goal_planner_enabled: Option<bool>,
    /// When `Some(true)`, enable the goal summarizer remotely (the one-shot
    /// closing "what was accomplished" summary on a verified achievement).
    /// When `Some(false)`, force-disable it (kill-switch).
    /// Absent ⇒ default tracks goal mode (enabled iff goal mode is on).
    #[serde(default)]
    pub goal_summary_enabled: Option<bool>,
    /// Number of adversarial skeptics spawned per goal-verification
    /// attempt (step ② of the staged gate). Clamped to `1..=5` at the
    /// resolver. Absent ⇒ harness default of
    /// `goal_classifier::GOAL_VERIFIER_SKEPTIC_COUNT` (3 today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_verifier_count: Option<u32>,
    /// Maximum per-goal classifier runs before the goal auto-pauses
    /// (BackOff). Clamped to `1..=10` at the resolver. Absent ⇒ harness
    /// default of `goal_classifier::GOAL_CLASSIFIER_MAX_RUNS_DEFAULT`
    /// (3 today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_classifier_max_runs: Option<u32>,
    /// Fire the stall-triggered strategist every N consecutive
    /// `NotAchieved` verifications. Clamped to `>= 1` at the resolver.
    /// Absent ⇒ default of `max(1, goal_classifier_max_runs / 2)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_strategist_every: Option<u32>,
    /// Planner role model+toolset. Absent ⇒ inherit current model. A
    /// present-but-malformed value is tolerantly dropped to `None` (not a
    /// hard parse error) so it cannot nuke the whole `RemoteSettings`
    /// payload (see [`deserialize_tolerant_goal_role_model`]).
    #[serde(
        default,
        deserialize_with = "deserialize_tolerant_goal_role_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub goal_planner_model: Option<GoalRoleModel>,
    /// Strategist role model+toolset. Absent ⇒ inherit current model. A
    /// present-but-malformed value is tolerantly dropped to `None`
    /// (see [`deserialize_tolerant_goal_role_model`]).
    #[serde(
        default,
        deserialize_with = "deserialize_tolerant_goal_role_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub goal_strategist_model: Option<GoalRoleModel>,
    /// Ordered skeptic pool. `pool[0]` = skeptic-0's model; skeptics
    /// `1..N` are assigned round-robin over the pool. Empty/absent ⇒
    /// inherit the current model. A single malformed pool entry is
    /// dropped rather than discarding the whole pool (see
    /// [`deserialize_tolerant_goal_skeptic_models`]).
    #[serde(
        default,
        deserialize_with = "deserialize_tolerant_goal_skeptic_models",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub goal_skeptic_models: Vec<GoalRoleModel>,
    #[serde(default)]
    pub workflows_enabled: Option<bool>,
    /// Remote fallback for managed MCP connector fetching.
    #[serde(default)]
    pub managed_mcps_enabled: Option<bool>,
    #[serde(default)]
    pub managed_mcp_gateway_tools_enabled: Option<bool>,
    /// Remote-policy disable lever for the **external OTEL** stream (customer
    /// collectors); feeds `ExternalOtelRemotePolicy.force_disable`.
    /// Restrictive-only by construction: there is deliberately
    /// no `external_otel_enabled` remote field — remote settings are fetched
    /// per-run and never persisted, so a remote "enable" could never reach
    /// init; org-wide enable ships via managed config instead. Applied
    /// in-process (tighten-only) via
    /// `echo_agent_telemetry::external::apply_remote_policy`.
    #[serde(default)]
    pub external_otel_disabled: Option<bool>,
    /// Force the external stream's content gates (`OTEL_LOG_USER_PROMPTS`,
    /// `OTEL_LOG_TOOL_DETAILS`) off regardless of local env/config.
    /// Tighten-only, like `external_otel_disabled`.
    #[serde(default)]
    pub external_otel_content_gates_locked: Option<bool>,
    /// `Some(false)` disarms managed-config signature verification (remote kill-switch).
    #[serde(default)]
    pub managed_config_signature_verification: Option<bool>,
    #[serde(default)]
    pub telemetry_enabled: Option<bool>,
    /// Telemetry mode override (string): `"session-metrics"`, `"full"`, `"off"`.
    /// Takes precedence over `telemetry_enabled` (bool) when present.
    #[serde(default)]
    pub telemetry_mode: Option<String>,
    #[serde(default)]
    pub trace_upload_enabled: Option<bool>,
    /// Enable user-facing feedback (heuristic popups, `/feedback` command).
    /// Session analytics (signal sync, turn deltas) are gated separately
    /// by `telemetry_enabled`.
    #[serde(default)]
    pub feedback_enabled: Option<bool>,
    /// Gradual rollout of the `/feedback` trace-consent card.
    #[serde(default)]
    pub feedback_trace_card_enabled: Option<bool>,
    /// Two-pass (prefire) compaction. When approaching the auto-compact
    /// threshold the shell speculatively summarizes the history prefix in the
    /// background (pass 1 → NOTE₁); at compaction it summarizes NOTE₁ + the
    /// recent tail (pass 2 → final summary), keeping summarizer latency off the
    /// critical path. `Some(true)` enables (remote rollout), `Some(false)` forces
    /// off, `None` falls back to `[features] two_pass_compaction` /
    /// `ECHO_AGENT_TWO_PASS_COMPACTION` / default (off).
    #[serde(default)]
    pub two_pass_compaction_enabled: Option<bool>,
    /// Dynamic tip list from remote settings. When present with non-empty entries,
    /// one tip is shown at startup (rotated daily by UTC day).
    /// `None` or `[]` = no tips shown.
    #[serde(default)]
    pub tips: Option<Vec<String>>,
    /// Free-form per-command tags (e.g. `new`, `beta`) rendered as a bracketed
    /// label in the slash dropdown, keyed by canonical command name. Present-but-
    /// malformed → `None` (does not fail the whole parse); local
    /// `[slash_command_tags]` overrides per key. See `resolve_slash_command_tags`.
    #[serde(default, deserialize_with = "deserialize_tolerant")]
    pub slash_command_tags: Option<std::collections::BTreeMap<String, String>>,
    /// When present, controls the non-Git-repo warning at session start.
    /// Controlled via remote settings (`non_git_warning` in `echo_agent_build_settings`).
    /// Takes precedence over `[features] non_git_warning` in config.toml:
    /// `Some(true)` enables, `Some(false)` acts as a kill-switch, `None` falls back to local config.
    #[serde(default)]
    pub non_git_warning: Option<bool>,
    /// remote settings gate for first-run auto-registration of the official EchoAgent
    /// marketplace source. `Some(true)` enables, `Some(false)` is a kill-switch,
    /// `None` falls back to env/default (off).
    #[serde(default)]
    pub official_marketplace_auto_register: Option<bool>,
    /// remote settings gate for the inline plugin-install CTA (keyword-matched
    /// marketplace upsell above the prompt). `Some(true)` enables, `Some(false)`
    /// is a kill-switch, `None` falls back to env/default (off).
    #[serde(default)]
    pub plugin_cta: Option<bool>,
    /// Remote announcements list from proxy. Malformed items are skipped entirely.
    /// `None` or `[]` = no announcements to display.
    #[serde(default, deserialize_with = "deserialize_tolerant_announcements")]
    pub announcements: Option<Vec<RemoteAnnouncement>>,
    #[serde(default)]
    pub web_search_model: Option<String>,
    #[serde(default)]
    pub session_summary_model: Option<String>,
    #[serde(default)]
    pub image_description_model: Option<String>,
    /// Server-side pin for the next-prompt suggestion model (tab-autocomplete
    /// ghost text), from the `echo_agent_build_settings` remote settings flag. Sits below
    /// env (`ECHO_AGENT_PROMPT_SUGGESTIONS_MODEL`) and `[models] prompt_suggestion`
    /// in config.toml, above the client hint and the built-in
    /// `echo-agent-build-0.1` default. The effective model is catalog-guarded: when
    /// it is not in the shell's model catalog the suggestion request is
    /// skipped entirely (never the session model). See
    /// `ModelOverrideConfig::resolve` and `handle_suggest_prompt`.
    #[serde(default)]
    pub prompt_suggestion_model: Option<String>,
    /// Server-recommended default model ID for new sessions.
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub campaigns: Vec<CampaignOverride>,
    /// When `Some(true)`, foreground commands that hit the default timeout are
    /// auto-backgrounded instead of killed. Fallback when no local
    /// `[toolset.bash] auto_background_on_timeout` is set in config.toml.
    #[serde(default)]
    pub auto_background_on_timeout: Option<bool>,
    /// When `Some(false)`, foreground commands containing a background `&`
    /// operator are rejected. Fallback when no local `[toolset.bash]
    /// allow_background_operator` is set; absent → client default (allow).
    #[serde(default)]
    pub allow_background_operator: Option<bool>,
    /// remote settings fallback for `[toolset.ask_user_question] timeout_enabled`.
    /// When `Some(false)`, questionnaires wait forever unless a higher tier
    /// (requirements / env / user / managed config) sets otherwise.
    #[serde(default)]
    pub ask_user_question_timeout_enabled: Option<bool>,
    /// remote settings fallback for `[toolset.ask_user_question] timeout_secs`
    /// (positive seconds). Absent → client default (1800 / 30 minutes).
    #[serde(default)]
    pub ask_user_question_timeout_secs: Option<u64>,
    /// When `Some(true)`, a completed subagent's isolated worktree is snapshotted
    /// into a durable git ref and its directory deleted (resume rehydrates from
    /// the ref). Fallback when no local `[features] subagent_worktree_snapshot`
    /// is set in config.toml. Absent → default (**disabled** — ships dark).
    #[serde(default)]
    pub subagent_worktree_snapshot_enabled: Option<bool>,
    /// `image_gen` / `/imagine`. `None` → env / `[features]` / default on.
    #[serde(default)]
    pub image_gen_enabled: Option<bool>,
    /// remote settings flag: optional Imagine model override for `image_gen`.
    /// When present and non-empty, `image_gen` uses this model slug
    /// (e.g. `echo-agent-imagine-image`) instead of the default quality model
    /// (`echo-agent-imagine-image-quality`). Absent/empty → default model.
    #[serde(default)]
    pub image_gen_model_override: Option<String>,
    /// Optional Imagine model override for `image_edit`. Absent/empty → default.
    #[serde(default)]
    pub image_edit_model_override: Option<String>,
    /// Video tools / `/imagine-video`. `None` → env / `[features]` / default on.
    #[serde(default)]
    pub video_gen_enabled: Option<bool>,
    /// When `Some(true)`, enable the process-wide image normalize cache that
    /// amortises decode + integrity-check + re-encode work across SessionActors.
    /// Default: disabled. See `session::normalize_cache`.
    #[serde(default)]
    pub image_normalize_cache_enabled: Option<bool>,
    /// When `Some(true)`, enrich path-not-found errors with CWD reminders,
    /// "did you mean?" corrections, and similar-name suggestions.
    /// When `Some(false)` or absent, error messages are unchanged.
    #[serde(default)]
    pub path_not_found_hints: Option<bool>,
    /// Remote enable tier for the per-tip contextual hints. Each field is a
    /// soft default for one tip: `Some(false)` disables, `Some(true)` enables,
    /// absent/null ⇒ client default (on). User config beats this tier.
    #[serde(default)]
    pub contextual_hints: Option<ContextualHintsRemote>,
    /// Server-recommended worktree creation type. Fallback when no local
    /// `[cli] worktree_type` is set in config.toml.
    #[serde(default)]
    pub worktree_type: Option<String>,
    /// Grove-projected worktree strategy (`true` = grove-fuse/grove-nfs, `false` = copy).
    /// `Some(false)` is the remote kill switch. `nfs_worktree` is a deserialize alias.
    #[serde(default, alias = "nfs_worktree")]
    pub grove_worktree: Option<bool>,
    /// Server-recommended default for `restore_code` in worktree resume.
    /// Applied only when the client omits `restoreCode`.
    #[serde(default)]
    pub restore_code: Option<bool>,
    /// When `Some(true)`, Ctrl+C before the first server activity rewinds
    /// the prompt back into the input box instead of cancelling the turn.
    #[serde(default)]
    pub cancel_rewind_enabled: Option<bool>,
    /// Enables the session recap feature (`/recap` + automatic return-from-away).
    /// Optional remote kill-switch; shell defaults ON when unset (set `false` to disable).
    #[serde(default)]
    pub session_recap: Option<bool>,
    /// Enables the session search index (`/load` deep search).
    /// Optional remote kill-switch; shell defaults ON when unset (set `false` to disable).
    #[serde(default)]
    pub session_search: Option<bool>,
    /// Enables the per-turn dashboard summary (one-line "what happened last
    /// turn" generated at turn end). Optional remote kill-switch; shell
    /// defaults ON when unset (set `false` to disable).
    #[serde(default)]
    pub turn_summary: Option<bool>,
    /// Enables the early-session auto-title refresh (regenerate the title from
    /// the whole conversation at a couple of early turns, then freeze). Optional
    /// remote kill-switch; when unset the shell defers to `turn_summary` so the
    /// two sibling post-turn side-calls default together without being coupled.
    #[serde(default)]
    pub title_refresh: Option<bool>,
    /// Enables the `ask_user_question` tool. Optional remote kill-switch:
    /// `Some(false)` strips the tool; `Some(true)` or absent → the shell
    /// default (ON). Feature-flagged via remote settings.
    #[serde(default)]
    pub ask_user_question_enabled: Option<bool>,
    /// When `Some(true)`, enable the `web_fetch` tool.
    /// When `Some(false)` or absent, the tool is not registered.
    /// Feature-flagged via remote settings for gradual rollout.
    #[serde(default)]
    pub web_fetch_enabled: Option<bool>,
    /// Egress proxy endpoint for the web_fetch tool.
    /// Fallback when no local `[toolset.web_fetch] proxy_endpoint` is set.
    #[serde(default)]
    pub web_fetch_proxy: Option<String>,
    /// Domain allowlist for the web_fetch tool.
    /// Fallback when no local `[toolset.web_fetch] allowed_domains` is set.
    #[serde(default)]
    pub web_fetch_allowed_domains: Option<Vec<String>>,
    /// When `Some(false)`, hide the resolved model ID in /session-info.
    #[serde(default)]
    pub show_resolved_model: Option<bool>,
    /// When `Some(true)`, enable session sharing.
    /// When `Some(false)` or absent, sharing is disabled.
    #[serde(default)]
    pub sharing_enabled: Option<bool>,
    /// Voice mode (STT dictation). Client default is **on** when absent.
    /// `Some(false)` is a remote kill switch; `Some(true)` forces on.
    /// Overridable locally via `ECHO_AGENT_VOICE_MODE`. Free-tier EchoAgent upsell
    /// is a separate client tier gate.
    #[serde(default)]
    pub voice_mode_enabled: Option<bool>,
    /// Whether ZDR (Zero Data Retention) users are allowed to use the product.
    /// Controlled via remote settings. Default `false` (blocked) during beta.
    #[serde(default)]
    pub zdr_access_enabled: Option<bool>,
    /// When `Some(true)`, the client may show the coding-data sharing upsell
    /// banner. Controlled via remote settings (`privacy_notice_rollout`).
    /// Absent/`None` means off so older servers and missing flags keep the
    /// banner hidden.
    #[serde(default)]
    pub privacy_notice_rollout: Option<bool>,
    /// Days after a privacy-banner dismiss before it may re-show for users who
    /// remain coding-data opted-out. From `echo_agent_build_settings`.
    /// `None` / `0` = never re-show after dismiss.
    #[serde(default)]
    pub privacy_banner_reshow_days: Option<u64>,
    /// remote settings tier of the `remember_tool_approvals` gate (whether per-tool
    /// "Always allow …" prompt options are shown). Lowest precedence; typically
    /// targeted per-org. Default `true`; `Some(false)` is a kill-switch.
    #[serde(default)]
    pub remember_tool_approvals: Option<bool>,
    /// remote settings tier of the crash-handler install gate. Lowest precedence in
    /// `resolve_crash_handler_enabled`; default off. `Some(false)` is a kill-switch.
    #[serde(default)]
    pub crash_handler_enabled: Option<bool>,
    /// Whether the TUI shows agent thinking/reasoning blocks in scrollback.
    /// `None` defers to local config / env / default (`true`).
    /// `Some(false)` is a remote kill-switch. Resolved via
    /// `resolve_show_thinking_blocks` (requirements > env > user > managed >
    /// remote > default true).
    #[serde(default)]
    pub show_thinking_blocks: Option<bool>,
    /// Whether the TUI folds runs of consecutive non-destructive tool calls
    /// (reads/searches/lists) into one transcript row. `None` defers to local
    /// config / env / default (`true`). `Some(false)` is a remote
    /// kill-switch. Resolved via `resolve_group_tool_verbs` (requirements >
    /// env > user > managed > remote > default true).
    #[serde(default)]
    pub group_tool_verbs: Option<bool>,
    /// Whether the TUI shows Edit tool calls as a collapsed one-line `+N/-M`
    /// diffstat summary by default and merges back-to-back edits to the same
    /// file into one row (expand for the diffs). `None` defers to local
    /// config / env / default (`false`); `Some(false)` is a remote kill
    /// switch. Resolved via `resolve_collapsed_edit_blocks` (requirements >
    /// env > user > managed > remote > default false). Explicit pager.toml
    /// `[scrollback.blocks.edit]` shape keys override the flag's fold shape
    /// client-side; merging always follows the flag.
    #[serde(default)]
    pub collapsed_edit_blocks: Option<bool>,
    /// Display-refresh probe + auto-cadence. See [`DisplayRefreshSettings`].
    /// Partial object falls through per-field; resolved via `resolve_display_refresh`.
    #[serde(default)]
    pub display_refresh: Option<DisplayRefreshSettings>,
    /// Raw remote settings JSON for the `[auto_mode]` table (gate `enabled`,
    /// `prompt_type`, `classifier_model`). Coerced into the shell's typed
    /// `AutoModeConfig` (config-types stays dependency-light). Lowest-precedence
    /// layer in `resolve_auto_permission_mode_enabled` (client default ON).
    #[serde(default)]
    pub auto_mode: Option<serde_json::Value>,
    /// Soft default permission mode (`"ask"` / `"auto"` / `"always-approve"` /
    /// `"default"`). Used only when no effective TOML permission key is set.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// User's subscription tier from remote settings `echo_agent_build_access_gate`.
    /// E.g. "free", "premium", "echoagent", "echoagent_heavy".
    /// Stamped on analytics events + user profile for filtering.
    #[serde(default)]
    pub subscription_tier: Option<String>,
    #[serde(default)]
    pub gate_message: Option<String>,
    #[serde(default)]
    pub gate_url: Option<String>,
    #[serde(default)]
    pub gate_label: Option<String>,
    /// A usable `id`, non-empty `body` and positive `version` arm it; anything else fails open.
    #[serde(default, deserialize_with = "deserialize_tolerant")]
    pub consent_gate: Option<ConsentGate>,
    /// Whether the session picker groups entries by repo name.
    /// When `None` or `Some(false)`, sessions are shown in a flat list.
    #[serde(default)]
    pub session_picker_grouped: Option<bool>,
    /// Whether the user is allowed to use EchoAgent Build. Set by remote settings
    /// `echo_agent_build_access_gate` targeting rules. `None` = no server response
    /// yet (client uses own fallback check). `Some(false)` = blocked.
    #[serde(default)]
    pub allow_access: Option<bool>,
    /// User-friendly display name for the current subscription tier
    /// (e.g. "EchoAgent", "X Premium+", "Free", "API Key"). Set by CCP
    /// from the JWT tier claim (OAuth) or credential kind (API key).
    /// Free/Invalid OAuth → `"Free"`; API keys → `"API Key"` (Mixpanel
    /// `api_key`, never free).
    #[serde(default)]
    pub subscription_tier_display: Option<String>,
    /// Whether on-demand credit usage is enabled. When `Some(false)`, the
    /// billing extension blocks on-demand cap changes.
    #[serde(default)]
    pub on_demand_enabled: Option<bool>,
    /// When set to a non-empty URL, the pager's `/usage` command shows a link
    /// to that URL instead of fetching billing data from the backend.
    /// Server-controlled via the remote settings `echo_agent_build_usage_redirect_url`
    /// feature flag (target it at personal-team users). `None`/empty keeps the
    /// default behaviour of fetching usage from the backend.
    #[serde(default)]
    pub usage_billing_redirect_url: Option<String>,
    /// Enable the shell command suggestion pipeline remotely.
    #[serde(default)]
    pub suggestions_enabled: Option<bool>,
    /// Enable AI-powered shell command suggestions remotely.
    #[serde(default)]
    pub suggestions_ai_enabled: Option<bool>,
    /// Global auto-compact threshold percent (0-100) from remote settings
    /// `echo_agent_build_settings`. Per-model override on `ModelInfo`
    /// (`echo_agent_build_models`) takes precedence; user config and env var
    /// further override per the resolver chain.
    #[serde(default)]
    pub auto_compact_threshold_percent: Option<u8>,
    /// Max subagent nesting depth (`echo_agent_build_settings.subagents_max_depth`).
    #[serde(default)]
    pub subagents_max_depth: Option<u32>,
    #[serde(default)]
    pub subagents_max_concurrent: Option<u32>,
    #[serde(default)]
    pub subagents_sampling_limit: Option<u32>,
    /// `"queue"` or `"fail"`.
    #[serde(default)]
    pub subagents_limit_behavior: Option<String>,
    #[serde(default)]
    pub workflow_max_concurrent_agents: Option<u32>,
    /// Max parallel `image_gen` / `image_edit` tool calls in one model step
    /// (`echo_agent_build_settings.max_parallel_image_gen_calls`).
    #[serde(default)]
    pub max_parallel_image_gen_calls: Option<u32>,
    /// Max parallel video-gen tool calls in one model step
    /// (`echo_agent_build_settings.max_parallel_video_gen_calls`).
    #[serde(default)]
    pub max_parallel_video_gen_calls: Option<u32>,
    /// Global system-prompt identity label. Per-model override wins; see
    /// `resolve_system_prompt_label`.
    #[serde(default)]
    pub system_prompt_label: Option<String>,
    /// Global per-compaction wall-clock budget (seconds) from remote settings;
    /// `0` disables. Env (`ECHO_AGENT_COMPACTION_WALL_CLOCK_SECS`) overrides it.
    /// Resolved via `resolve_compaction_wall_clock_budget_secs`.
    #[serde(default)]
    pub compaction_wall_clock_budget_secs: Option<u64>,
    /// Compaction mode (`summary` | `transcript` | `segments`) from remote settings.
    /// Env (`ECHO_AGENT_COMPACTION_MODE`) and user config override it.
    #[serde(default)]
    pub compaction_mode: Option<String>,
    /// Segments verbatim detail (`none` | `minimal` | `balanced` | `verbose`)
    /// from remote settings. Env (`ECHO_AGENT_COMPACTION_DETAIL`) and config override it.
    #[serde(default)]
    pub compaction_detail: Option<String>,
    /// remote settings verbatim-input flag; env (`ECHO_AGENT_COMPACTION_VERBATIM_INPUT`) and config override it. `None` = default (true).
    #[serde(default)]
    pub compaction_verbatim_input: Option<bool>,
    #[serde(default)]
    pub compaction_tool_choice: Option<String>,
    /// remote settings denylist of optional imagine tools to disable
    /// (e.g. `["image_edit"]`). When a tool is listed it is authoritatively
    /// removed from the toolset and local env/config can't re-enable it.
    /// Absent or not listed → each tool keeps its own default.
    /// See `Config::resolve_image_edit`.
    #[serde(default)]
    pub imagine_tools_disabled: Option<Vec<String>>,
    /// remote settings gate for the `echoagent workspace` CLI command (Computer Hub
    /// workspace exposure), from `echo_agent_build_settings.workspace_command_enabled`.
    /// `Some(true)` enables it; `None`/`Some(false)` (the default) keep it off.
    #[serde(default)]
    pub workspace_command_enabled: Option<bool>,
    #[serde(default)]
    pub workspace_dashboard_enabled: Option<bool>,
    /// Soft default for `keep_text_selection` (`"flash"` / `"hold"` / `"word_select"`), from
    /// `echo_agent_build_settings.keep_text_selection_default`. Applied only when the user has set no
    /// local text-selection preference; an explicit local `keep_text_selection` always wins. An
    /// absent or unrecognized value keeps the client default (`flash`). Set remotely to stage a
    /// new default to a segment, cut everyone over, or revert it for a customer.
    #[serde(default)]
    pub keep_text_selection_default: Option<String>,
    /// Master switch for jemalloc heap sampling + threshold dumps.
    /// `Some(true)` enables, `Some(false)` kill-switch, `None` = client default off.
    #[serde(default)]
    pub jemalloc_heap_profile_enabled: Option<bool>,
    /// Resident-byte thresholds (e.g. 2G/5G/10G as byte counts).
    /// `None` and `[]` are distinct on the wire.
    #[serde(default)]
    pub jemalloc_heap_profile_thresholds_bytes: Option<Vec<u64>>,
    /// Stats poll interval in seconds when set.
    #[serde(default)]
    pub jemalloc_heap_profile_poll_interval_secs: Option<u64>,
}
impl RemoteSettings {
    /// Denylist check for an optional imagine tool. Returns `true` when the
    /// server sent `imagine_tools_disabled` and it contains `tool` (force-off);
    /// otherwise `false` (defer to the tool's own default).
    pub fn imagine_tool_disabled(&self, tool: &str) -> bool {
        self.imagine_tools_disabled
            .as_ref()
            .is_some_and(|list| list.iter().any(|t| t == tool))
    }
}
/// Remote enable tier for the per-tip contextual hints (mirrors the client's
/// `[ui.contextual_hints]` shape). Each field is a soft default for one tip;
/// `None` defers to the client default (on). All fields `#[serde(default)]` so
/// a partial object from remote settings never fails the whole `RemoteSettings` parse.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ContextualHintsRemote {
    /// Undo tip (Ctrl+Z after a substantial draft wipe).
    #[serde(default)]
    pub undo: Option<bool>,
    /// Plan-mode nudge (typing a planning keyword).
    #[serde(default)]
    pub plan_mode: Option<bool>,
    /// Clipboard-image input tip.
    #[serde(default)]
    pub image_input: Option<bool>,
    /// Send-now tip after queuing a mid-turn follow-up (InterjectPrompt chord).
    #[serde(default)]
    pub send_now: Option<bool>,
    /// Small-screen tip (`/compact-mode` hint on smallish terminals).
    #[serde(default)]
    pub small_screen: Option<bool>,
    /// Word-select tip after double-click fold/nav (settings discoverability).
    #[serde(default)]
    pub word_select: Option<bool>,
    /// SSH wrap session-load tip (recommend `echoagent wrap ssh` for remote sessions).
    #[serde(default)]
    pub ssh_wrap: Option<bool>,
}
/// Tolerant deserializer for `Option<Vec<RemoteAnnouncement>>`.
/// Parses as Vec<Value>, tries each as RemoteAnnouncement, drops failures.
/// This ensures one bad item does not poison the whole RemoteSettings.
/// Logs a warning when malformed items are dropped.
fn deserialize_tolerant_announcements<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<RemoteAnnouncement>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<serde_json::Value> = serde::Deserialize::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                match serde_json::from_value::<RemoteAnnouncement>(item) {
                    Ok(a) => out.push(a),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "remote settings announcements: dropped malformed item"
                        );
                    }
                }
            }
            Ok(Some(out))
        }
        Some(_) => Ok(None),
    }
}
/// Parse one JSON value as a [`GoalRoleModel`], returning `None` (with a
/// `tracing::warn!`) instead of erroring when the value is malformed.
/// Shared by the tolerant deserializers for the single-pair role fields
/// and the skeptic pool so all three goal-role-model fields drop bad
/// remote payloads rather than failing the whole `RemoteSettings` parse.
fn parse_goal_role_model_tolerant(value: serde_json::Value) -> Option<GoalRoleModel> {
    match serde_json::from_value::<GoalRoleModel>(value) {
        Ok(model) => Some(model),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "remote settings goal role model: dropped malformed value"
            );
            None
        }
    }
}
/// Tolerant deserializer for `Option<GoalRoleModel>` (the single-pair
/// role fields). Parses as `Option<Value>`; a present-but-malformed value
/// (or an explicit `null`) maps to `None` via
/// [`parse_goal_role_model_tolerant`] rather than erroring, so one bad
/// remote payload cannot nuke the whole `RemoteSettings` parse.
fn deserialize_tolerant_goal_role_model<'de, D>(
    deserializer: D,
) -> Result<Option<GoalRoleModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<serde_json::Value> = serde::Deserialize::deserialize(deserializer)?;
    Ok(match opt {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => parse_goal_role_model_tolerant(value),
    })
}
/// Tolerant deserializer for `Vec<GoalRoleModel>` (the skeptic pool).
/// Parses as `Option<Value>`; a non-array value (or null/absent) yields
/// an empty pool, and within an array each malformed entry is dropped
/// (via [`parse_goal_role_model_tolerant`]) instead of nuking the whole
/// pool. Survivor order is preserved — the skeptic round-robin assignment
/// (`expand_skeptic_assignment`) depends on pool order.
fn deserialize_tolerant_goal_skeptic_models<'de, D>(
    deserializer: D,
) -> Result<Vec<GoalRoleModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<serde_json::Value> = serde::Deserialize::deserialize(deserializer)?;
    match opt {
        Some(serde_json::Value::Array(arr)) => Ok(arr
            .into_iter()
            .filter_map(parse_goal_role_model_tolerant)
            .collect()),
        _ => Ok(Vec::new()),
    }
}
/// A model + the harness whose system prompt / toolset flavor that model must
/// run against. The pair is the atomic configurable unit because a model is
/// only guaranteed to work with a compatible harness (cursor vs echo-agent-build).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRoleModel {
    /// Model id, e.g. "echo-agent-4". Resolved against available models at
    /// spawn time; unknown/unauthorized ⇒ fail-open to current model.
    pub model: String,
    /// Harness `agent_type` (e.g. "cursor", "echo-agent-build-plan") whose
    /// `AgentDefinition` decides the role subagent's harness flavor (system
    /// prompt + cursor-vs-echo-agent-build toolset), applied REGARDLESS of the
    /// session/parent agent. Resolved by NAME (project/plugin/builtin lookup,
    /// then re-flavored by the subagent toolset resolver) — NOT via the main
    /// session's env/ACP/strict-harness precedence chain. NOT a subagent type:
    /// the role always spawns `general-purpose`, so the harness only re-flavors
    /// that toolset. An `agent_type` that doesn't resolve, that resolves to a
    /// strict harness whose flavor the subagent system can't represent (e.g.
    /// `codex`), or whose role toolset can't satisfy the role, fails open to the
    /// session model + harness before commit.
    pub agent_type: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worktree_auto_gc_partial_object_and_round_trip() {
        let json = r#"{"worktree_auto_gc":{"enabled":false}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        let agc = s.worktree_auto_gc.as_ref().expect("present");
        assert_eq!(agc.enabled, Some(false));
        assert_eq!(agc.max_age_secs, None);
        assert_eq!(agc.min_interval_secs, None);
        assert_eq!(agc.dry_run, None);
        assert_eq!(agc.include_orphan_snapshots, None);
        assert_eq!(agc.max_age_by_kind, None);
        let full = r#"{
            "worktree_auto_gc": {
                "enabled": true,
                "max_age_secs": 604800,
                "min_interval_secs": 21600,
                "dry_run": true,
                "include_orphan_snapshots": false,
                "max_age_by_kind": {
                    "session": 604800,
                    "subagent": 86400,
                    "manual": "never"
                }
            }
        }"#;
        let s: RemoteSettings = serde_json::from_str(full).unwrap();
        let agc = s.worktree_auto_gc.clone().unwrap();
        let mut kind_map = std::collections::BTreeMap::new();
        kind_map.insert("session".into(), WorktreeKindMaxAge::Secs(604800));
        kind_map.insert("subagent".into(), WorktreeKindMaxAge::Secs(86400));
        kind_map.insert("manual".into(), WorktreeKindMaxAge::Never);
        assert_eq!(
            agc,
            WorktreeAutoGcSettings {
                enabled: Some(true),
                max_age_secs: Some(604800),
                min_interval_secs: Some(21600),
                dry_run: Some(true),
                include_orphan_snapshots: Some(false),
                max_age_by_kind: Some(kind_map),
                include_rebuild: None,
                rebuild_min_interval_secs: None,
            }
        );
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.worktree_auto_gc, s.worktree_auto_gc);
        let absent: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.worktree_auto_gc, None);
        let extra = r#"{"worktree_auto_gc":{"enabled":true,"future_knob":1}}"#;
        let s: RemoteSettings = serde_json::from_str(extra).unwrap();
        assert_eq!(s.worktree_auto_gc.unwrap().enabled, Some(true));
        let partial_bad = r#"{"worktree_auto_gc":{"enabled":false,"max_age_secs":"nope"}}"#;
        let s: RemoteSettings = serde_json::from_str(partial_bad).unwrap();
        let agc = s.worktree_auto_gc.as_ref().expect("object still present");
        assert_eq!(agc.enabled, Some(false));
        assert_eq!(agc.max_age_secs, None);
        let kind_partial = r#"{
            "worktree_auto_gc": {
                "max_age_by_kind": {
                    "subagent": 86400,
                    "session": {"nested": true},
                    "manual": "never"
                }
            }
        }"#;
        let s: RemoteSettings = serde_json::from_str(kind_partial).unwrap();
        let map = s
            .worktree_auto_gc
            .as_ref()
            .and_then(|a| a.max_age_by_kind.as_ref())
            .expect("map present");
        assert_eq!(map.get("subagent"), Some(&WorktreeKindMaxAge::Secs(86400)));
        assert_eq!(map.get("manual"), Some(&WorktreeKindMaxAge::Never));
        assert!(!map.contains_key("session"));
        let null_never =
            r#"{"worktree_auto_gc":{"max_age_by_kind":{"manual":null,"pool":172800}}}"#;
        let s: RemoteSettings = serde_json::from_str(null_never).unwrap();
        let map = s.worktree_auto_gc.unwrap().max_age_by_kind.unwrap();
        assert_eq!(map.get("manual"), Some(&WorktreeKindMaxAge::Never));
        assert_eq!(map.get("pool"), Some(&WorktreeKindMaxAge::Secs(172800)));
        assert_eq!(
            serde_json::to_value(&WorktreeKindMaxAge::Never).unwrap(),
            serde_json::Value::String("never".into())
        );
        let nested_bad = r#"{"leader_mode":true,"worktree_auto_gc":"not-an-object"}"#;
        let s: RemoteSettings = serde_json::from_str(nested_bad).unwrap();
        assert_eq!(s.leader_mode, Some(true));
        assert_eq!(s.worktree_auto_gc, None);
    }
    #[test]
    fn remote_settings_vendor_sessions_round_trip_and_default_absent() {
        let session_flags = |settings: &RemoteSettings| {
            (
                settings.cursor_sessions_enabled,
                settings.claude_sessions_enabled,
                settings.codex_sessions_enabled,
            )
        };
        let json = r#"{
            "cursor_sessions_enabled": true,
            "claude_sessions_enabled": false,
            "codex_sessions_enabled": true
        }"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            session_flags(&settings),
            (Some(true), Some(false), Some(true))
        );
        let serialized = serde_json::to_string(&settings).unwrap();
        let round_trip: RemoteSettings = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            session_flags(&round_trip),
            (Some(true), Some(false), Some(true))
        );
        let absent: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(session_flags(&absent), (None, None, None));
    }
    #[test]
    fn remote_settings_image_description_model_round_trip() {
        let json = r#"{"image_description_model": "echo-agent-build"}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.image_description_model.as_deref(),
            Some("echo-agent-build")
        );
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.image_description_model, s.image_description_model);
    }
    #[test]
    fn remote_settings_prompt_suggestion_model_round_trip() {
        let json = r#"{"prompt_suggestion_model": "echo-agent-build-0.1"}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.prompt_suggestion_model.as_deref(),
            Some("echo-agent-build-0.1")
        );
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.prompt_suggestion_model, s.prompt_suggestion_model);
        let s3: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s3.prompt_suggestion_model, None);
    }
    #[test]
    fn remote_settings_announcements_absent() {
        let json = r#"{}"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.announcements, None);
    }
    #[test]
    fn remote_settings_announcements_populated() {
        let json = r#"{"announcements": [{"id": "a", "message": "m"}]}"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            settings.announcements,
            Some(vec![RemoteAnnouncement {
                id: Some("a".to_string()),
                message: Some("m".to_string()),
                severity: None,
                title: None,
                cta: None,
                updated_at: None,
                expires_at: None,
                dismissible: None,
                persistent: None,
            }])
        );
    }
    #[test]
    fn remote_settings_announcements_one_bad_item_does_not_poison() {
        let json = r#"{
            "announcements": [
                {"id": "good", "message": "ok"},
                {"id": 999, "message": "bad-id-type"}
            ]
        }"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            settings.announcements,
            Some(vec![RemoteAnnouncement {
                id: Some("good".to_string()),
                message: Some("ok".to_string()),
                severity: None,
                title: None,
                cta: None,
                updated_at: None,
                expires_at: None,
                dismissible: None,
                persistent: None,
            }])
        );
    }
    #[test]
    fn remote_settings_consent_gate_round_trip() {
        let json = r#"{
            "consent_gate": {
                "id": "tos-2026-08",
                "version": 3,
                "title": "Updated terms",
                "body": "Review our [Terms of Service](https://echo-agent.invalid/legal/tos) before continuing.",
                "accept_label": "Accept and continue"
            }
        }"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        let gate = settings.consent_gate.expect("gate present");
        assert_eq!(gate.id, "tos-2026-08");
        assert_eq!(gate.version, Some(3));
        assert!(
            gate.body
                .as_deref()
                .is_some_and(|b| b.contains("https://echo-agent.invalid/legal/tos"))
        );
    }
    /// A poisoned response would leave `zdr_access_enabled` false and hard-block ZDR users.
    #[test]
    fn remote_settings_malformed_consent_gate_does_not_poison() {
        for gate in [
            r#"{"id": "tos", "version": "not-a-number"}"#,
            r#"{"version": 3, "body": "Review our terms."}"#,
        ] {
            let json = format!(r#"{{"consent_gate": {gate}, "tips": ["still parsed"]}}"#);
            let settings: RemoteSettings = serde_json::from_str(&json).unwrap();
            assert!(settings.consent_gate.is_none(), "{gate} must be dropped");
            assert_eq!(settings.tips, Some(vec!["still parsed".to_string()]));
        }
    }
    #[test]
    fn remote_settings_consent_gate_absent_is_none() {
        let settings: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.consent_gate.is_none());
    }
    #[test]
    fn remote_settings_goal_role_models_absent_default_clean() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_planner_model, None);
        assert_eq!(s.goal_strategist_model, None);
        assert!(s.goal_skeptic_models.is_empty());
    }
    #[test]
    fn remote_settings_goal_planner_model_round_trip() {
        let json =
            r#"{"goal_planner_model": {"model": "echo-agent-4", "agent_type": "general-purpose"}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_planner_model,
            Some(GoalRoleModel {
                model: "echo-agent-4".to_string(),
                agent_type: "general-purpose".to_string(),
            })
        );
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.goal_planner_model, s.goal_planner_model);
    }
    #[test]
    fn remote_settings_goal_strategist_model_round_trip() {
        let json =
            r#"{"goal_strategist_model": {"model": "echo-agent-4.5", "agent_type": "cursor"}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_strategist_model,
            Some(GoalRoleModel {
                model: "echo-agent-4.5".to_string(),
                agent_type: "cursor".to_string(),
            })
        );
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.goal_strategist_model, s.goal_strategist_model);
    }
    #[test]
    fn remote_settings_goal_skeptic_models_fully_valid_pool_round_trips() {
        let json = r#"{"goal_skeptic_models": [
            {"model": "echo-agent-4", "agent_type": "general-purpose"},
            {"model": "echo-agent-3", "agent_type": "cursor"}
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_skeptic_models,
            vec![
                GoalRoleModel {
                    model: "echo-agent-4".to_string(),
                    agent_type: "general-purpose".to_string(),
                },
                GoalRoleModel {
                    model: "echo-agent-3".to_string(),
                    agent_type: "cursor".to_string(),
                },
            ]
        );
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.goal_skeptic_models, s.goal_skeptic_models);
    }
    #[test]
    fn remote_settings_goal_skeptic_models_one_bad_item_does_not_poison_pool() {
        let json = r#"{"goal_skeptic_models": [
            {"model": "echo-agent-4", "agent_type": "general-purpose"},
            {"model": "echo-agent-broken"},
            {"model": "echo-agent-3", "agent_type": "cursor"}
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_skeptic_models,
            vec![
                GoalRoleModel {
                    model: "echo-agent-4".to_string(),
                    agent_type: "general-purpose".to_string(),
                },
                GoalRoleModel {
                    model: "echo-agent-3".to_string(),
                    agent_type: "cursor".to_string(),
                },
            ]
        );
    }
    #[test]
    fn remote_settings_goal_skeptic_models_null_yields_empty() {
        let json = r#"{"goal_skeptic_models": null}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert!(s.goal_skeptic_models.is_empty());
    }
    #[test]
    fn remote_settings_goal_skeptic_models_empty_array_yields_empty() {
        let json = r#"{"goal_skeptic_models": []}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert!(s.goal_skeptic_models.is_empty());
    }
    #[test]
    fn remote_settings_goal_skeptic_models_all_entries_bad_yields_empty() {
        let json = r#"{"goal_skeptic_models": [
            {"model": "only-model"},
            {"agent_type": "only-agent-type"},
            "scalar-not-an-object",
            42
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert!(s.goal_skeptic_models.is_empty());
    }
    #[test]
    fn remote_settings_goal_skeptic_models_non_array_yields_empty() {
        for json in [
            r#"{"goal_skeptic_models": {"model": "x", "agent_type": "y"}}"#,
            r#"{"goal_skeptic_models": "not-an-array"}"#,
            r#"{"goal_skeptic_models": 7}"#,
        ] {
            let s: RemoteSettings = serde_json::from_str(json).unwrap();
            assert!(
                s.goal_skeptic_models.is_empty(),
                "non-array pool must yield empty for {json}"
            );
        }
    }
    #[test]
    fn remote_settings_goal_skeptic_models_missing_model_entry_dropped() {
        let json = r#"{"goal_skeptic_models": [
            {"agent_type": "general-purpose"},
            {"model": "echo-agent-3", "agent_type": "cursor"}
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_skeptic_models,
            vec![GoalRoleModel {
                model: "echo-agent-3".to_string(),
                agent_type: "cursor".to_string(),
            }]
        );
    }
    #[test]
    fn remote_settings_goal_skeptic_models_wrong_typed_scalar_dropped() {
        let json = r#"{"goal_skeptic_models": [
            {"model": 123, "agent_type": "general-purpose"},
            {"model": "echo-agent-3", "agent_type": ["cursor"]},
            {"model": "echo-agent-4", "agent_type": "general-purpose"}
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_skeptic_models,
            vec![GoalRoleModel {
                model: "echo-agent-4".to_string(),
                agent_type: "general-purpose".to_string(),
            }]
        );
    }
    #[test]
    fn remote_settings_goal_skeptic_models_extra_unknown_fields_kept() {
        let json = r#"{"goal_skeptic_models": [
            {"model": "echo-agent-4", "agent_type": "general-purpose", "reasoning_effort": "high"}
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_skeptic_models,
            vec![GoalRoleModel {
                model: "echo-agent-4".to_string(),
                agent_type: "general-purpose".to_string(),
            }]
        );
    }
    #[test]
    fn remote_settings_goal_skeptic_models_survivor_order_preserved() {
        let json = r#"{"goal_skeptic_models": [
            {"model": "first", "agent_type": "general-purpose"},
            {"model": "bad"},
            {"model": "second", "agent_type": "cursor"},
            "garbage",
            {"model": "third", "agent_type": "general-purpose"}
        ]}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        let order: Vec<&str> = s
            .goal_skeptic_models
            .iter()
            .map(|m| m.model.as_str())
            .collect();
        assert_eq!(order, vec!["first", "second", "third"]);
    }
    #[test]
    fn remote_settings_goal_planner_model_malformed_yields_none() {
        for json in [
            r#"{"goal_planner_model": {"model": "only-model"}}"#,
            r#"{"goal_planner_model": {"agent_type": "only-agent-type"}}"#,
            r#"{"goal_planner_model": {"model": 1, "agent_type": "x"}}"#,
            r#"{"goal_planner_model": "scalar"}"#,
            r#"{"goal_planner_model": null}"#,
        ] {
            let s: RemoteSettings = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("must not hard-error for {json}: {e}"));
            assert_eq!(s.goal_planner_model, None, "for {json}");
        }
    }
    #[test]
    fn remote_settings_goal_strategist_model_malformed_yields_none() {
        for json in [
            r#"{"goal_strategist_model": {"model": "only-model"}}"#,
            r#"{"goal_strategist_model": {"agent_type": ["x"]}}"#,
            r#"{"goal_strategist_model": 42}"#,
        ] {
            let s: RemoteSettings = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("must not hard-error for {json}: {e}"));
            assert_eq!(s.goal_strategist_model, None, "for {json}");
        }
    }
    #[test]
    fn remote_settings_goal_role_models_malformed_pair_does_not_drop_other_fields() {
        let json = r#"{
            "goal_planner_model": {"model": "broken"},
            "goal_strategist_model": {"model": "echo-agent-4.5", "agent_type": "cursor"},
            "default_model": "echo-agent-4"
        }"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_planner_model, None);
        assert_eq!(
            s.goal_strategist_model,
            Some(GoalRoleModel {
                model: "echo-agent-4.5".to_string(),
                agent_type: "cursor".to_string(),
            })
        );
        assert_eq!(s.default_model.as_deref(), Some("echo-agent-4"));
    }
    #[test]
    fn remote_settings_goal_role_model_extra_unknown_fields_kept_single_pair() {
        let json = r#"{"goal_planner_model": {"model": "echo-agent-4", "agent_type": "general-purpose", "future": true}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.goal_planner_model,
            Some(GoalRoleModel {
                model: "echo-agent-4".to_string(),
                agent_type: "general-purpose".to_string(),
            })
        );
    }
    #[test]
    fn remote_settings_inference_idle_timeout_present() {
        let json = r#"{"inference_idle_timeout_secs": 180}"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.inference_idle_timeout_secs, Some(180));
    }
    #[test]
    fn remote_settings_initial_injection_deserialize_present() {
        let json = r#"{"memory_initial_injection_enabled": false, "memory_initial_injection_min_score": 0.66}"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.memory_initial_injection_enabled, Some(false));
        assert_eq!(settings.memory_initial_injection_min_score, Some(0.66));
    }
    #[test]
    fn remote_settings_initial_injection_deserialize_absent() {
        let json = r#"{"memory_enabled": true}"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.memory_initial_injection_enabled, None);
        assert_eq!(settings.memory_initial_injection_min_score, None);
    }
    #[test]
    fn remote_settings_inference_idle_timeout_absent() {
        let json = r#"{"memory_enabled": true}"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.inference_idle_timeout_secs, None);
    }
    #[test]
    fn remote_settings_unknown_fields_tolerated() {
        let json = r#"{
            "inference_idle_timeout_secs": 120,
            "future_remote_field": 42,
            "verification_staleness_enabled": true
        }"#;
        let settings: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.inference_idle_timeout_secs, Some(120));
    }
    #[test]
    fn remote_settings_web_fetch_fields_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.web_fetch_enabled, None);
        assert_eq!(s.web_fetch_proxy, None);
        assert_eq!(s.web_fetch_allowed_domains, None);
    }
    #[test]
    fn remote_settings_web_fetch_all_populated() {
        let json = r#"{
            "web_fetch_enabled": true,
            "web_fetch_proxy": "https://proxy.corp.example.com",
            "web_fetch_allowed_domains": ["docs.rs", "example.com"]
        }"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.web_fetch_enabled, Some(true));
        assert_eq!(
            s.web_fetch_proxy.as_deref(),
            Some("https://proxy.corp.example.com")
        );
        assert_eq!(
            s.web_fetch_allowed_domains,
            Some(vec!["docs.rs".to_owned(), "example.com".to_owned()])
        );
    }
    #[test]
    fn remote_settings_web_fetch_enabled_false() {
        let json = r#"{"web_fetch_enabled": false}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.web_fetch_enabled, Some(false));
        assert_eq!(s.web_fetch_proxy, None);
    }
    #[test]
    fn remote_settings_ask_user_question_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.ask_user_question_enabled, None);
    }
    #[test]
    fn remote_settings_ask_user_question_enabled_round_trip() {
        let json = r#"{"ask_user_question_enabled": true}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.ask_user_question_enabled, Some(true));
        let out = serde_json::to_string(&s).unwrap();
        let s2: RemoteSettings = serde_json::from_str(&out).unwrap();
        assert_eq!(s2.ask_user_question_enabled, Some(true));
    }
    #[test]
    fn remote_settings_ask_user_question_enabled_false() {
        let json = r#"{"ask_user_question_enabled": false}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.ask_user_question_enabled, Some(false));
    }
    #[test]
    fn remote_settings_web_fetch_empty_domains() {
        let json = r#"{"web_fetch_allowed_domains": []}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.web_fetch_allowed_domains, Some(vec![]));
    }
    #[test]
    fn remote_settings_display_refresh_present_partial() {
        let json = r#"{"display_refresh": {"auto_cadence_enabled": true, "floor_ms": 7}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        let dr = s.display_refresh.expect("display_refresh present");
        assert_eq!(dr.auto_cadence_enabled, Some(true));
        assert_eq!(dr.floor_ms, Some(7));
        assert_eq!(dr.probe_enabled, None);
        assert_eq!(dr.ceiling_ms, None);
        assert_eq!(dr.min_hz, None);
        assert_eq!(dr.max_hz, None);
    }
    #[test]
    fn remote_settings_display_refresh_absent() {
        let s: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.display_refresh, None);
    }
    #[test]
    fn remote_settings_display_refresh_unknown_keys_preserved() {
        let json =
            r#"{"display_refresh": {"probe_enabled": true, "future_knob": 42, "floor_ms": "bad"}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        let dr = s.display_refresh.expect("display_refresh present");
        assert_eq!(dr.probe_enabled, Some(true));
        assert_eq!(dr.floor_ms, None, "wrong-typed floor_ms ignored");
        assert_eq!(dr.extra.get("future_knob"), Some(&serde_json::json!(42)));
        let out = serde_json::to_value(&dr).unwrap();
        assert_eq!(out.get("future_knob"), Some(&serde_json::json!(42)));
        assert_eq!(out.get("probe_enabled"), Some(&serde_json::json!(true)));
    }
    #[test]
    fn remote_settings_contextual_hints_present() {
        let json = r#"{"contextual_hints": {"undo": false, "plan_mode": true}}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        let hints = s.contextual_hints.expect("contextual_hints present");
        assert_eq!(hints.undo, Some(false));
        assert_eq!(hints.plan_mode, Some(true));
        assert_eq!(hints.image_input, None);
    }
    #[test]
    fn remote_settings_contextual_hints_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.contextual_hints, None);
    }
    #[test]
    fn remote_settings_workflows_flag_round_trips() {
        let settings: RemoteSettings =
            serde_json::from_str(r#"{"workflows_enabled": true}"#).unwrap();
        assert_eq!(settings.workflows_enabled, Some(true));
        let round: RemoteSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(round.workflows_enabled, Some(true));
    }
    #[test]
    fn remote_settings_goal_planner_enabled_present() {
        let json = r#"{"goal_planner_enabled": true}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_planner_enabled, Some(true));
    }
    #[test]
    fn remote_settings_goal_planner_enabled_false() {
        let json = r#"{"goal_planner_enabled": false}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_planner_enabled, Some(false));
    }
    #[test]
    fn remote_settings_goal_planner_enabled_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_planner_enabled, None);
    }
    #[test]
    fn remote_settings_goal_summary_enabled_present() {
        let json = r#"{"goal_summary_enabled": true}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_summary_enabled, Some(true));
    }
    #[test]
    fn remote_settings_goal_summary_enabled_false() {
        let json = r#"{"goal_summary_enabled": false}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_summary_enabled, Some(false));
    }
    #[test]
    fn remote_settings_goal_summary_enabled_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.goal_summary_enabled, None);
    }
    #[test]
    fn remote_settings_folder_trust_enabled_present() {
        let json = r#"{"folder_trust_enabled": true}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.folder_trust_enabled, Some(true));
    }
    #[test]
    fn remote_settings_folder_trust_enabled_false() {
        let json = r#"{"folder_trust_enabled": false}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.folder_trust_enabled, Some(false));
    }
    #[test]
    fn remote_settings_folder_trust_enabled_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.folder_trust_enabled, None);
    }
    #[test]
    fn remote_settings_workspace_command_enabled_present() {
        let json = r#"{"workspace_command_enabled": true}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.workspace_command_enabled, Some(true));
    }
    #[test]
    fn remote_settings_workspace_command_enabled_false() {
        let json = r#"{"workspace_command_enabled": false}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.workspace_command_enabled, Some(false));
    }
    #[test]
    fn remote_settings_workspace_command_enabled_absent() {
        let json = r#"{}"#;
        let s: RemoteSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.workspace_command_enabled, None);
    }
    #[test]
    fn remote_settings_workspace_dashboard_enabled_parses_all_states() {
        let on: RemoteSettings =
            serde_json::from_str(r#"{"workspace_dashboard_enabled": true}"#).unwrap();
        assert_eq!(on.workspace_dashboard_enabled, Some(true));
        let off: RemoteSettings =
            serde_json::from_str(r#"{"workspace_dashboard_enabled": false}"#).unwrap();
        assert_eq!(off.workspace_dashboard_enabled, Some(false));
        let absent: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.workspace_dashboard_enabled, None);
    }
    #[test]
    fn remote_settings_keep_text_selection_default_round_trips() {
        let ws: RemoteSettings =
            serde_json::from_str(r#"{"keep_text_selection_default": "word_select"}"#).unwrap();
        assert_eq!(
            ws.keep_text_selection_default.as_deref(),
            Some("word_select")
        );
        let flash: RemoteSettings =
            serde_json::from_str(r#"{"keep_text_selection_default": "flash"}"#).unwrap();
        assert_eq!(flash.keep_text_selection_default.as_deref(), Some("flash"));
        let absent: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.keep_text_selection_default, None);
    }
    #[test]
    fn remote_settings_permission_mode_deserializes() {
        let s: RemoteSettings = serde_json::from_str(r#"{"permission_mode": "auto"}"#).unwrap();
        assert_eq!(s.permission_mode.as_deref(), Some("auto"));
        let s2: RemoteSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s2.permission_mode.as_deref(), Some("auto"));
        let s: RemoteSettings =
            serde_json::from_str(r#"{"permission_mode": "always-approve"}"#).unwrap();
        assert_eq!(s.permission_mode.as_deref(), Some("always-approve"));
        let s: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.permission_mode, None);
        let s: RemoteSettings = serde_json::from_str(r#"{"permission_mode": null}"#).unwrap();
        assert_eq!(s.permission_mode, None);
    }
    #[test]
    fn remote_settings_crash_handler_enabled_present() {
        let s: RemoteSettings = serde_json::from_str(r#"{"crash_handler_enabled": true}"#).unwrap();
        assert_eq!(s.crash_handler_enabled, Some(true));
        let s2: RemoteSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s2.crash_handler_enabled, Some(true));
    }
    #[test]
    fn remote_settings_crash_handler_enabled_false() {
        let s: RemoteSettings =
            serde_json::from_str(r#"{"crash_handler_enabled": false}"#).unwrap();
        assert_eq!(s.crash_handler_enabled, Some(false));
    }
    #[test]
    fn remote_settings_crash_handler_enabled_absent() {
        let s: RemoteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.crash_handler_enabled, None);
    }
    type JemallocFields<'a> = (Option<bool>, Option<&'a [u64]>, Option<u64>);
    fn jemalloc_fields(s: &RemoteSettings) -> JemallocFields<'_> {
        (
            s.jemalloc_heap_profile_enabled,
            s.jemalloc_heap_profile_thresholds_bytes.as_deref(),
            s.jemalloc_heap_profile_poll_interval_secs,
        )
    }
    fn parse_remote(json: &str) -> RemoteSettings {
        serde_json::from_str(json).unwrap_or_else(|e| panic!("parse failed for {json}: {e}"))
    }
    fn round_trip_remote(s: &RemoteSettings) -> RemoteSettings {
        let out = serde_json::to_string(s).unwrap();
        parse_remote(&out)
    }
    fn assert_jemalloc_round_trip(json: &str, expected: JemallocFields<'_>) {
        let s = parse_remote(json);
        assert_eq!(jemalloc_fields(&s), expected);
        assert_eq!(jemalloc_fields(&round_trip_remote(&s)), expected);
    }
    fn assert_remote_parse_err(json: &str) {
        assert!(
            serde_json::from_str::<RemoteSettings>(json).is_err(),
            "expected parse error for {json}"
        );
    }
    #[test]
    fn remote_settings_privacy_notice_rollout_absent_null_true_false() {
        assert_eq!(parse_remote("{}").privacy_notice_rollout, None);
        assert_eq!(
            parse_remote(r#"{"privacy_notice_rollout": null}"#).privacy_notice_rollout,
            None
        );
        let on = parse_remote(r#"{"privacy_notice_rollout": true}"#);
        assert_eq!(on.privacy_notice_rollout, Some(true));
        assert_eq!(round_trip_remote(&on).privacy_notice_rollout, Some(true));
        let off = parse_remote(r#"{"privacy_notice_rollout": false}"#);
        assert_eq!(off.privacy_notice_rollout, Some(false));
        assert_eq!(round_trip_remote(&off).privacy_notice_rollout, Some(false));
    }
    #[test]
    fn remote_settings_privacy_banner_reshow_days() {
        assert_eq!(parse_remote("{}").privacy_banner_reshow_days, None);
        assert_eq!(
            parse_remote(r#"{"privacy_banner_reshow_days": 30}"#).privacy_banner_reshow_days,
            Some(30)
        );
        let s = parse_remote(r#"{"privacy_banner_reshow_days": 7}"#);
        assert_eq!(round_trip_remote(&s).privacy_banner_reshow_days, Some(7));
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_fields_absent_and_null() {
        assert_eq!(jemalloc_fields(&parse_remote("{}")), (None, None, None));
        assert_eq!(
            jemalloc_fields(&parse_remote(
                r#"{
                    "jemalloc_heap_profile_enabled": null,
                    "jemalloc_heap_profile_thresholds_bytes": null,
                    "jemalloc_heap_profile_poll_interval_secs": null
                }"#
            )),
            (None, None, None)
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_enabled_true_false_round_trip() {
        assert_jemalloc_round_trip(
            r#"{"jemalloc_heap_profile_enabled": true}"#,
            (Some(true), None, None),
        );
        assert_jemalloc_round_trip(
            r#"{"jemalloc_heap_profile_enabled": false}"#,
            (Some(false), None, None),
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_kill_switch_with_non_empty_thresholds() {
        assert_jemalloc_round_trip(
            r#"{
                "jemalloc_heap_profile_enabled": false,
                "jemalloc_heap_profile_thresholds_bytes": [2147483648, 5368709120],
                "jemalloc_heap_profile_poll_interval_secs": 30
            }"#,
            (Some(false), Some(&[2_147_483_648, 5_368_709_120]), Some(30)),
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_thresholds_populated_and_empty_round_trip() {
        assert_jemalloc_round_trip(
            r#"{
                "jemalloc_heap_profile_thresholds_bytes": [2147483648, 5368709120, 10737418240]
            }"#,
            (
                None,
                Some(&[2_147_483_648, 5_368_709_120, 10_737_418_240]),
                None,
            ),
        );
        assert_jemalloc_round_trip(
            r#"{"jemalloc_heap_profile_thresholds_bytes": []}"#,
            (None, Some(&[]), None),
        );
        assert_jemalloc_round_trip(
            r#"{
                "jemalloc_heap_profile_enabled": true,
                "jemalloc_heap_profile_thresholds_bytes": []
            }"#,
            (Some(true), Some(&[]), None),
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_poll_interval_round_trip() {
        assert_jemalloc_round_trip(
            r#"{"jemalloc_heap_profile_poll_interval_secs": 60}"#,
            (None, None, Some(60)),
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_all_fields_populated_round_trip() {
        assert_jemalloc_round_trip(
            r#"{
                "jemalloc_heap_profile_enabled": true,
                "jemalloc_heap_profile_thresholds_bytes": [2147483648, 5368709120],
                "jemalloc_heap_profile_poll_interval_secs": 15
            }"#,
            (Some(true), Some(&[2_147_483_648, 5_368_709_120]), Some(15)),
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_boundary_u64_values() {
        let json = format!(
            r#"{{
                "jemalloc_heap_profile_thresholds_bytes": [0, 1, {}],
                "jemalloc_heap_profile_poll_interval_secs": {}
            }}"#,
            u64::MAX,
            u64::MAX
        );
        assert_jemalloc_round_trip(&json, (None, Some(&[0, 1, u64::MAX]), Some(u64::MAX)));
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_coexists_with_unknown_keys() {
        let s = parse_remote(
            r#"{
                "jemalloc_heap_profile_enabled": true,
                "jemalloc_heap_profile_thresholds_bytes": [1073741824],
                "jemalloc_heap_profile_poll_interval_secs": 30,
                "future_jemalloc_knob": 42,
                "unrelated_remote_field": "ok"
            }"#,
        );
        assert_eq!(
            jemalloc_fields(&s),
            (Some(true), Some([1_073_741_824].as_slice()), Some(30))
        );
    }
    #[test]
    fn remote_settings_jemalloc_heap_profile_malformed_fails_whole_parse() {
        for json in [
            r#"{"jemalloc_heap_profile_enabled": "yes"}"#,
            r#"{"jemalloc_heap_profile_enabled": 1}"#,
            r#"{"jemalloc_heap_profile_enabled": []}"#,
            r#"{"jemalloc_heap_profile_enabled": {}}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": "2G"}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": {"bytes": 1}}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": [true]}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": ["2G"]}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": [-1]}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": [null]}"#,
            r#"{"jemalloc_heap_profile_thresholds_bytes": [1, null, 2]}"#,
            r#"{"jemalloc_heap_profile_poll_interval_secs": "30"}"#,
            r#"{"jemalloc_heap_profile_poll_interval_secs": true}"#,
            r#"{"jemalloc_heap_profile_poll_interval_secs": -1}"#,
            r#"{
                "jemalloc_heap_profile_enabled": true,
                "jemalloc_heap_profile_thresholds_bytes": "bad",
                "workspace_command_enabled": true
            }"#,
        ] {
            assert_remote_parse_err(json);
        }
    }
}
