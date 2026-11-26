//! Legacy (0.4.10) behavior for `read_file`.
//!
//! Centralizes all version-specific policy decisions for legacy-0.4.10:
//! - Generic error message for all filesystem failures (no structured variants)
//! - No gitignore enforcement (gitignored files are readable)
//!
//! The main `run_read_file()` flow calls these helpers to make version-specific
//! decisions. The execution path stays in `mod.rs`; the policy lives here.

use std::path::Path;

/// Exact historical read failure message for `read_file` in legacy-0.4.10.
///
/// Captured from the historical 0.4.10 implementation.
///
/// Historical 0.4.10 collapsed filesystem read failures (missing file,
/// directory path, permission denied, etc.) into the same generic message
/// without appending OS error detail.
pub(crate) fn render_read_error(path: &Path) -> String {
    format!("Failed to read file: {}", path.display())
}

/// Legacy 0.4.10 does not enforce gitignore — gitignored files are readable.
pub(crate) fn allows_gitignored_reads() -> bool {
    true
}
