//! Version-specific behavior modules for `read_file`.
//!
//! - `legacy_0_4_10`: generic error messages, no gitignore enforcement,
//!   legacy marker for reminder suppression.
//! - Current behavior remains in `read_file/mod.rs` (structured error
//!   variants, gitignore enforcement, confusable reminders).

pub(crate) mod legacy_0_4_10;
