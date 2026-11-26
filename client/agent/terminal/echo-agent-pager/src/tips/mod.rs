//! Ephemeral tip primitive: a single-slot, TTL'd hint line rendered in the
//! banner rect above the prompt input.
//!
//! Unlike the toast, an ephemeral tip deliberately survives typing — it is
//! cleared only by TTL expiry, prompt-box submission, or an explicit clear.
//! Tips carrying a seen-count key are show-gated by the app-level, per-session
//! seen-count map (`AppView::tip_seen_counts`) so they stop appearing once seen
//! often enough within a run; that map is in-memory only and resets each run.

pub mod clear_detector;
pub mod clipboard_focus;
pub mod ephemeral;
pub mod plan_nudge;
pub mod render;
pub mod send_now;
pub mod small_screen;
pub mod ssh_wrap;
pub mod word_select;

pub use ephemeral::{DEFAULT_TIP_TICKS, EphemeralTip, EphemeralTipState, tip_row_renderable};
