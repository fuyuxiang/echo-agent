//! Bazel runfiles helpers for locating test data.
//!
//! Under `bazel test`, source files and test data are accessed via the
//! *runfiles* tree.  Under `cargo test`, `CARGO_MANIFEST_DIR` provides
//! the crate root.  The [`crate_root!`] macro abstracts over both.

use std::path::PathBuf;

/// Try to resolve a runfiles path to an absolute directory.
///
/// Returns `Some(path)` when running under Bazel (with the `bazel` feature
/// enabled) and the runfiles entry exists, `None` otherwise.
pub fn try_resolve_runfiles(_path: &str) -> Option<PathBuf> {
    #[cfg(feature = "bazel")]
    {
        let r = runfiles::Runfiles::create().ok()?;
        runfiles::rlocation!(r, _path)
    }
    #[cfg(not(feature = "bazel"))]
    {
        None
    }
}

/// Resolve the crate root directory, working under both `bazel test` and
/// `cargo test`.
///
/// Under Bazel the path is resolved via runfiles; under Cargo it falls back
/// to `CARGO_MANIFEST_DIR`.
///
/// # Example
///
/// ```ignore
/// use echo_agent_test_utils::crate_root;
///
/// fn test_data_dir() -> std::path::PathBuf {
///     crate_root!("_main/crates/echo-agent-test-utils").join("testdata")
/// }
/// ```
#[macro_export]
macro_rules! crate_root {
    ($runfiles_path:expr) => {
        $crate::runfiles_util::try_resolve_runfiles($runfiles_path)
            .unwrap_or_else(|| ::std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")))
    };
}
