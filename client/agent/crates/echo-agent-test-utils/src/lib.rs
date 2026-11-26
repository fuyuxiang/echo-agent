//! Shared test utilities for EchoAgent crates.
//!
//! Provides common helpers that are needed by many crates' test suites:
//!
//! - **Hermetic git**: [`git::ensure_hermetic_git_on_path`] prepends the Bazel-provided
//!   static `git` binary to `PATH` so that tests don't depend on a system-installed git.
//!   The [`require_git!`] macro is a convenient shorthand.
//!
//! - **Git repo helpers**: [`git::init_git_repo`] and [`git::git_commit_all`] for
//!   setting up throwaway git repos in tests.
//!
//! - **Bazel runfiles**: [`crate_root!`] resolves the crate root directory via
//!   Bazel runfiles (for `bazel test`) or `CARGO_MANIFEST_DIR` (for `cargo test`).
//!
//! - **Tracing capture**: [`tracing_capture::MessagePrefixCounter`] counts
//!   log lines by message prefix (thread-scoped or global install) for tests
//!   that assert on how often an instrumented code path ran.
//!
//! - **Env knobs**: [`env::env_usize`] for perf-repro test sizing.

pub mod env;
pub mod git;
pub mod image;
pub mod runfiles_util;
pub mod tracing_capture;
