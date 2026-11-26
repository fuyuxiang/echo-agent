//! Plugin system — discover, load, and manage plugins (including compat layouts).
//!
//! A plugin is a self-contained directory that bundles skills, agents,
//! MCP server configs, and hooks into a namespaced unit.  Plugins can
//! live under `~/.echo-agent/plugins/`, `.echo-agent/plugins/` (project-level),
//! or be passed via `--plugin-dir` on the CLI.
//!
//! This module handles:
//! - `manifest` — parsing `plugin.json` manifests
//! - `discovery` — scanning the filesystem for plugin directories
//! - `trust` — project-plugin trust management
//! - `registry` — in-memory registry of active plugins

pub mod discovery;
pub mod git_install;
pub mod hooks_adapter;
pub mod install_registry;
pub mod local_refresh;
pub mod manifest;
pub mod marketplace;
pub mod marketplace_scope;
pub mod registry;
pub mod trust;

pub use discovery::{
    DiscoveredPlugin, PluginOrigin, PluginScope, discover_plugins, project_plugin_dirs,
    project_plugin_dirs_in,
};
pub use hooks_adapter::parse_plugin_hooks;
pub use install_registry::InstallRegistry;
pub use manifest::PluginManifest;
pub use registry::{LoadedPlugin, PluginRegistry, SharedPluginRegistryHandle};
pub use trust::TrustStore;
