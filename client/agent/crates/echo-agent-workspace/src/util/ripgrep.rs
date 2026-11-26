// Resolution (bundled binary, RG_BIN_PATH, Bazel runfiles, PATH) lives in the
// echo-agent-tools crate; this module only preserves the `crate::util::ripgrep` path.
pub use echo_agent_tools::util::rg_path;
