//! Version-specific behavior modules for `search_replace`.
//!
//! - `legacy_0_4_10`: error downgrade logic that restores exact historical
//!   0.4.10 wording by collapsing structured error variants to `InvalidInput`.
//! - Current behavior remains in `search_replace/mod.rs` (structured errors,
//!   gitignore enforcement, confusable diagnostics).

pub(crate) mod legacy_0_4_10;
