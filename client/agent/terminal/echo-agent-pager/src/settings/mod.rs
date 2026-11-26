//! Settings registry and modal — the canonical place for user preferences.
//!
//! The settings system is a pure-metadata registry of every user-tunable
//! preference in the pager. See `registry.rs` for the data model and
//! `defs.rs` for the catalog of registered settings.
//!
//! ## Architecture
//!
//! - **`SettingMeta`** is pure metadata: no function pointers, no closures,
//!   no `dyn`. Each entry declares its key, category, owner, kind, and
//!   discoverability metadata (label, description, keywords).
//! - **Defaults** come from `UiConfig::default()` for SHELL/SHARED settings;
//!   pager-supplied for PAGER-owned.
//! - **Reads** snapshot the live `UiConfig` (and pager-local state for
//!   PAGER-owned). The modal carries the snapshot for the duration of one
//!   open session.
//! - **Writes** are typed `Action::SetX(value)` variants dispatched
//!   directly by the modal — no `compute()` indirection, no `Vec<Action>`
//!   factories. Persistence goes through `Effect::PersistSetting`, which
//!   routes to `echo_agent_runtime::util::config::set_<field>(value).await`.
//!
//! The registry is built once in `main` and threaded through
//! `AppView::settings_registry: Arc<SettingsRegistry>` (mirroring the
//! existing `ActionRegistry` pattern). No `LazyLock`, no global state.

pub mod defs;
pub mod registry;

pub use registry::{
    CodingDataSharingLock, DynamicEnumSource, EnumChoice, OwnedEnumChoice, PagerLocalSnapshot,
    SettingCategory, SettingKey, SettingKind, SettingMeta, SettingOwner, SettingValue,
    SettingsRegistry, StringValidator, canonical_hunk_tracker_mode, canonical_screen_mode,
    canonical_voice_capture_mode, canonical_voice_stt_language, current_value_for,
    default_value_for, dynamic_enum_choices, is_consent_chooser,
};
