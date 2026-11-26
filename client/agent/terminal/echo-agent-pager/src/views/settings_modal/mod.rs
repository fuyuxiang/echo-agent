//! Settings modal — opens via F2, `/settings`, command palette, and
//! shortcuts-help.
//!
//! ## State machine
//!
//! `SettingsModalState` carries a `UiConfig` snapshot plus a mode machine:
//!
//! - `Browse` — j/k navigates rows; Space toggles Bool; Enter opens
//!   a chooser/editor for Enum/String/Int.
//! - `FilterFocused` — `/` enters filter mode; `invalidate_filter`
//!   recomputes `filtered_cache` on every mutation.
//! - `PickingEnum { ... }` — enum chooser sub-pane.
//! - `EditingValue { ... }` — inline string/int editor.
//!
//! ## Keyboard ↔ mouse parity
//!
//! Every keyboard interaction has a mouse equivalent via `handle_mouse`.
//!
//! ## Close-key interception
//!
//! F2/Ctrl+,/Cmd+, are intercepted before mode-specific routing.
//! Esc-in-Browse is handled by the `ModalWindow` chrome (so
//! `is_close_key` does NOT match Esc); Esc-in-FilterFocused exits
//! filter mode without closing.

mod input;
mod render;
mod state;

#[cfg(test)]
mod tests;

pub use input::{handle_settings_key, handle_settings_mouse, handle_settings_paste};
pub use render::{ResetConfirmOverlay, render_settings_modal};
#[allow(unused_imports)] // re-export for crate path; used by settings/registry tests
pub(crate) use state::MAX_PICKER_CHOICES;
pub use state::{MODAL_TITLE, RowEntry, SettingsKeyOutcome, SettingsModalMode, SettingsModalState};
