//! Stable `tracing` target for workspace telemetry events.
//!
//! Every tool_state / environment telemetry event routes through [`dc_log!`]
//! onto the [`TELEMETRY_TARGET`] target. Only the closed, structured
//! field set below is emitted on this target:
//!
//! - `session_id`, `turn_number`, `phase`
//! - `bytes`, `file_count`, `pending`, `pending_bytes`, `sample_period_secs`
//! - `error_category`, `outcome`
//! - the guaranteed-literal discriminators `skip_reason` / `drain_reason`
//!   (enum `…::as_str()` values only)
//! - the drain counters `grace_ms` / `active_at_start` / `pending_at_start` /
//!   `producers_at_start`
//!
//! Free-form `reason` / `error` names are deliberately never emitted here.

/// `tracing` target for all workspace telemetry events.
pub(crate) const TELEMETRY_TARGET: &str = "workspace::telemetry";

/// Emit a telemetry `tracing` event on [`TELEMETRY_TARGET`].
///
/// Thin wrapper over the leveled `tracing` macros with the target pinned, so a
/// call site can't accidentally land an event on a different target. `$level`
/// is a `tracing` level-macro name (`info`, `warn`, …); the remaining tokens
/// are the usual `tracing` fields + message. Use only the field vocabulary
/// documented on this module.
macro_rules! dc_log {
    ($level:ident, $($rest:tt)*) => {
        ::tracing::$level!(target: $crate::telemetry::TELEMETRY_TARGET, $($rest)*)
    };
}
pub(crate) use dc_log;
