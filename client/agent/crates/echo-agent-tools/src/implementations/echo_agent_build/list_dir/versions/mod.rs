//! Version-specific behavior modules for `list_dir`.
//!
//! - `legacy_0_4_10`: depth-threshold rendering + generic error messages
//! - Current behavior remains in `list_dir/mod.rs` (BFS budget rendering
//!   + structured error variants).

pub(crate) mod legacy_0_4_10;
