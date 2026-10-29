//! Type-safe heterogeneous resource container for the new tool architecture.
//!
//! `Resources` is the typed dependency injection container. It provides a
//! single `HashMap<TypeId, Box<dyn Any>>` that tools read from and write to.
//!
//! ## Design
//!
//! - **Typed access**: `get::<T>()`, `get_mut::<T>()`, `insert::<T>(val)`.
//! - **Params vs State**: `Params<T>` and `State<T>` are wrappers with distinct
//!   `TypeId`s so a tool's config and runtime state can coexist.
//! - **Serialization**: Registered types (via `register_params` / `register_state`)
//!   are serialized by category (`"params"` / `"state"`). Ephemeral types
//!   (e.g., `Cwd`) are silently skipped.
//! - **String-keyed access**: `get_json` / `set_json` for dynamic access by
//!   category + key string — used by the gRPC `SetToolOptions` / `GetToolOptions`
//!   RPCs.
use crate::computer::types::{AsyncFileSystem, TerminalBackend};
use crate::notification::types::ToolNotificationHandle;
use serde::Serialize;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
/// Marker trait for types that can be stored in `Resources`.
///
/// Each implementor must provide a unique `ID` string of the form
/// `"namespace.Name"` (e.g., `"echo_agent_build.ReadFile"`). The ID is used as
/// the serialization key when persisting resources.
///
/// Use the `register_resource!` macro to implement this.
pub trait ResourceType: Any + 'static {
    /// Unique identifier, e.g. `"echo_agent_build.ReadFile"`.
    const ID: &'static str;
    /// Additional semantic validation for finalize-time params.
    fn validate_params_value(
        _: &Self,
    ) -> Result<(), crate::types::params_validation::ParamValidationError> {
        Ok(())
    }
}
/// `()` implements `ResourceType` with an empty ID.
/// Used as `type Params = ()` for tools that have no configuration.
impl ResourceType for () {
    const ID: &'static str = "";
}
/// Implement `ResourceType` for a type with an explicit namespace and name.
///
/// ```ignore
/// register_resource!("echo_agent_build", "ReadFile", ReadHistory);
/// ```
///
/// This generates:
/// ```ignore
/// impl ResourceType for ReadHistory {
///     const ID: &'static str = "echo_agent_build.ReadFile";
/// }
/// ```
#[macro_export]
macro_rules! register_resource {
    ($namespace:literal, $name:literal, $ty:ty) => {
        impl $crate::types::resources::ResourceType for $ty {
            const ID: &'static str = concat!($namespace, ".", $name);
        }
    };
}
/// Wrapper for tool *configuration* / *parameters* stored in Resources.
///
/// `Params<T>` and `State<T>` have distinct `TypeId`s even for the same `T`,
/// so a tool's config and runtime state can coexist without collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params<T>(pub T);
impl<T: Default> Default for Params<T> {
    fn default() -> Self {
        Self(T::default())
    }
}
impl<T> std::ops::Deref for Params<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> std::ops::DerefMut for Params<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
impl<T: Serialize> Serialize for Params<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Params<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Params)
    }
}
/// Wrapper for tool *runtime state* stored in Resources.
///
/// `State<T>` has a distinct `TypeId` from `Params<T>`, enabling both to
/// coexist in the same `Resources` container for the same inner type `T`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State<T>(pub T);
impl<T: Default> Default for State<T> {
    fn default() -> Self {
        Self(T::default())
    }
}
impl<T> std::ops::Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> std::ops::DerefMut for State<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
impl<T: Serialize> Serialize for State<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for State<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(State)
    }
}
/// Category for a registered resource — determines the top-level key in
/// serialized output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceCategory {
    Params,
    State,
}
impl ResourceCategory {
    fn as_str(&self) -> &'static str {
        match self {
            ResourceCategory::Params => "params",
            ResourceCategory::State => "state",
        }
    }
}
/// Type-erased serialize closure for a registered resource.
type SerializeFn = Box<dyn Fn(&(dyn Any + Send + Sync)) -> Option<serde_json::Value> + Send + Sync>;
/// Type-erased deserialize closure for a registered resource.
type DeserializeFn =
    Box<dyn Fn(serde_json::Value, &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>) + Send + Sync>;
/// Metadata for a registered (serializable) resource.
///
/// Stores the `TypeId`, string key, category, and type-erased
/// serialize/deserialize closures so `Resources` can round-trip through JSON.
struct ResourceEntry {
    type_id: TypeId,
    /// The `ResourceType::ID` string (e.g., `"echo_agent_build.ReadFile"`).
    id: String,
    category: ResourceCategory,
    /// Serialize the value stored at `type_id` to JSON.
    serialize_fn: SerializeFn,
    /// Deserialize a JSON value and insert it into the `data` map.
    deserialize_fn: DeserializeFn,
}
/// Type-safe heterogeneous container for tool resources.
///
/// Stores typed values indexed by `TypeId`. Registered types are serializable;
/// ephemeral types (inserted directly without registration) are skipped during
/// serialization.
///
/// All stored values must be `Send + Sync` so `Resources` itself is
/// `Send + Sync`. This is required because `ToolRegistry` (which owns
/// `Resources`) may be wrapped in a `RwLock` or `Mutex` by multi-threaded
/// hosts.
pub struct Resources {
    /// The actual storage: `TypeId` → `Box<dyn Any + Send + Sync>`.
    data: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    /// Registered (serializable) entries.
    entries: Vec<ResourceEntry>,
}
pub type SharedResources = Arc<Mutex<Resources>>;
impl Default for Resources {
    fn default() -> Self {
        Self::new()
    }
}
impl Resources {
    /// Create an empty resource container.
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
            entries: Vec::new(),
        }
    }
    /// Wrap this `Resources` into a `SharedResources` (`Arc<Mutex<Resources>>`).
    pub fn into_shared(self) -> SharedResources {
        Arc::new(Mutex::new(self))
    }
    /// Get a shared reference to a stored value.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.data
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }
    /// Get a shared reference to a stored value, or return
    /// a `custom("missing_resource", ...)` error with the type name if absent.
    pub fn require<T: Send + Sync + 'static>(
        &self,
    ) -> Result<&T, echo_agent_tool_runtime::ToolError> {
        self.get::<T>().ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::custom(
                "missing_resource",
                format!("missing required resource: {}", std::any::type_name::<T>()),
            )
        })
    }
    /// Get a mutable reference to a stored value.
    pub fn get_mut<T: Send + Sync + 'static>(&mut self) -> Option<&mut T> {
        self.data
            .get_mut(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_mut::<T>())
    }
    /// Get a mutable reference, inserting `T::default()` if not present.
    pub fn get_or_default<T: Default + Send + Sync + 'static>(&mut self) -> &mut T {
        self.data
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("TypeId collision: stored type doesn't match requested type")
    }
    /// Insert a typed value, replacing any existing value of the same type.
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.data.insert(TypeId::of::<T>(), Box::new(value));
    }
    /// Remove a typed value, returning it if it existed.
    pub fn remove<T: Send + Sync + 'static>(&mut self) -> Option<T> {
        self.data
            .remove(&TypeId::of::<T>())
            .and_then(|boxed| (boxed as Box<dyn Any>).downcast::<T>().ok())
            .map(|boxed| *boxed)
    }
    /// Check if a value of type `T` is present.
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.data.contains_key(&TypeId::of::<T>())
    }
    /// Register a `Params<T>` type for serialization under the `"params"` category.
    ///
    /// After registration, `Params<T>` values will be included in `serialize()`
    /// output and can be restored via `load_from()`.
    pub fn register_params<T>(&mut self)
    where
        T: ResourceType
            + serde::Serialize
            + for<'de> serde::Deserialize<'de>
            + Default
            + Send
            + Sync
            + 'static,
    {
        let type_id = TypeId::of::<Params<T>>();
        let id = T::ID.to_string();
        if self.entries.iter().any(|e| e.type_id == type_id) {
            return;
        }
        self.entries.push(ResourceEntry {
            type_id,
            id,
            category: ResourceCategory::Params,
            serialize_fn: Box::new(|any: &(dyn Any + Send + Sync)| {
                any.downcast_ref::<Params<T>>()
                    .and_then(|p| serde_json::to_value(p).ok())
            }),
            deserialize_fn: Box::new(
                |val: serde_json::Value, data: &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>| {
                    if let Ok(p) = serde_json::from_value::<Params<T>>(val) {
                        data.insert(TypeId::of::<Params<T>>(), Box::new(p));
                    }
                },
            ),
        });
    }
    /// Register a `State<T>` type for serialization under the `"state"` category.
    ///
    /// After registration, `State<T>` values will be included in `serialize()`
    /// output and can be restored via `load_from()`.
    pub fn register_state<T>(&mut self)
    where
        T: ResourceType
            + serde::Serialize
            + for<'de> serde::Deserialize<'de>
            + Default
            + Send
            + Sync
            + 'static,
    {
        let type_id = TypeId::of::<State<T>>();
        let id = T::ID.to_string();
        if self.entries.iter().any(|e| e.type_id == type_id) {
            return;
        }
        self.entries.push(ResourceEntry {
            type_id,
            id,
            category: ResourceCategory::State,
            serialize_fn: Box::new(|any: &(dyn Any + Send + Sync)| {
                any.downcast_ref::<State<T>>()
                    .and_then(|s| serde_json::to_value(s).ok())
            }),
            deserialize_fn: Box::new(
                |val: serde_json::Value, data: &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>| {
                    if let Ok(s) = serde_json::from_value::<State<T>>(val) {
                        data.insert(TypeId::of::<State<T>>(), Box::new(s));
                    }
                },
            ),
        });
    }
    /// Serialize all registered resources to a nested JSON structure.
    ///
    /// Output shape:
    /// ```json
    /// {
    ///   "params": {
    ///     "echo_agent_build.Edit": { ... },
    ///   },
    ///   "state": {
    ///     "echo_agent_build.ReadFile": { ... },
    ///     "echo_agent_build.Todo": { ... },
    ///   }
    /// }
    /// ```
    ///
    /// Ephemeral types (not registered) are silently skipped.
    pub fn serialize(&self) -> serde_json::Value {
        let mut categories: HashMap<&str, serde_json::Map<String, serde_json::Value>> =
            HashMap::new();
        for entry in &self.entries {
            if let Some(boxed) = self.data.get(&entry.type_id)
                && let Some(val) = (entry.serialize_fn)(boxed.as_ref())
            {
                categories
                    .entry(entry.category.as_str())
                    .or_default()
                    .insert(entry.id.clone(), val);
            }
        }
        let mut top = serde_json::Map::new();
        for (cat, map) in categories {
            top.insert(cat.to_string(), serde_json::Value::Object(map));
        }
        serde_json::Value::Object(top)
    }
    /// Load registered resources from a previously serialized JSON structure.
    ///
    /// Expects the same shape as `serialize()` output:
    /// `{ "params": { ... }, "state": { ... } }`.
    ///
    /// Unknown keys are silently ignored. Missing keys leave the resource
    /// at its current value (or absent).
    pub fn load_from(&mut self, data: HashMap<String, HashMap<String, serde_json::Value>>) {
        for entry in &self.entries {
            let category_key = entry.category.as_str();
            if let Some(cat_map) = data.get(category_key)
                && let Some(val) = cat_map.get(&entry.id)
            {
                (entry.deserialize_fn)(val.clone(), &mut self.data);
            }
        }
    }
    /// Get a registered resource's value as JSON, by category and key.
    ///
    /// Used by the gRPC `GetToolOptions` RPC for dynamic access.
    pub fn get_json(&self, category: &str, key: &str) -> Option<serde_json::Value> {
        for entry in &self.entries {
            if entry.category.as_str() == category && entry.id == key {
                if let Some(boxed) = self.data.get(&entry.type_id) {
                    return (entry.serialize_fn)(boxed.as_ref());
                }
                return None;
            }
        }
        None
    }
    /// Set a registered resource's value from JSON, by category and key.
    ///
    /// Used by the gRPC `SetToolOptions` RPC for dynamic access.
    /// Returns `true` if a matching registration was found and the value was set.
    pub fn set_json(&mut self, category: &str, key: &str, val: serde_json::Value) -> bool {
        for entry in &self.entries {
            if entry.category.as_str() == category && entry.id == key {
                (entry.deserialize_fn)(val, &mut self.data);
                return true;
            }
        }
        false
    }
}
impl std::fmt::Debug for Resources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resources")
            .field("data_count", &self.data.len())
            .field("registered_entries", &self.entries.len())
            .finish()
    }
}
/// Current working directory for the session.
#[derive(Debug, Clone)]
pub struct Cwd(pub PathBuf);
/// Absolute path to the plan file for this session.
///
/// Set by the session layer (from `PlanModeTracker::plan_file_path()`);
/// read by `ExitPlanMode` to locate the plan on disk. When absent the
/// tool falls back to `Cwd/.echo-agent/plan.md`.
#[derive(Debug, Clone)]
pub struct PlanFilePath(pub PathBuf);
/// Default plan-file path (relative to the workspace root) used when no
/// explicit [`PlanFilePath`] is set. Shared by the plan-mode tools.
pub const PLAN_FILE_RELATIVE_PATH: &str = ".echo-agent/plan.md";
/// Resolve the session plan-file path from resources as `(absolute_target, display)`.
///
/// `absolute_target` is `Some` ONLY when the resolved path is absolute, so
/// callers that write/seed never create a file under the process CWD; it is
/// `None` for the display-only relative fallback. `display` is the
/// model-facing path string. Resolution: [`PlanFilePath`] (as-is), else
/// [`Cwd`]`/.echo-agent/plan.md`, else the bare relative `.echo-agent/plan.md`.
pub(crate) fn resolve_plan_file_path(res: &Resources) -> (Option<PathBuf>, String) {
    let path = if let Some(configured) = res.get::<PlanFilePath>() {
        configured.0.clone()
    } else if let Some(cwd) = res.get::<Cwd>() {
        cwd.0.join(PLAN_FILE_RELATIVE_PATH)
    } else {
        PathBuf::from(PLAN_FILE_RELATIVE_PATH)
    };
    let display = path.display().to_string();
    let absolute_target = path.is_absolute().then_some(path);
    (absolute_target, display)
}
/// Like [`resolve_plan_file_path`] but errors when no absolute target resolves.
pub(crate) fn require_plan_file_path(
    res: &Resources,
) -> Result<(PathBuf, String), echo_agent_tool_runtime::ToolError> {
    let (target, display) = resolve_plan_file_path(res);
    let target = target.ok_or_else(|| {
        echo_agent_tool_runtime::ToolError::custom(
            "missing_resource",
            "missing required resource: PlanFilePath or an absolute Cwd",
        )
    })?;
    Ok((target, display))
}
/// Stable display path for forked sessions.
///
/// When set, [`resolve_model_path`] rewrites absolute paths that start with
/// this prefix to the real [`Cwd`] (the on-disk worktree backing the fork).
/// This lets models keep using the original project path from conversation
/// history while all I/O hits the correct path on disk.
///
/// Inserted for forked sessions whose tool execution path differs from the
/// path the model should see.
#[derive(Debug, Clone)]
pub struct DisplayCwd(pub PathBuf);
/// Managed `Read`-deny glob patterns (e.g. `**/.env`, `**/*.pem`) from the
/// permission policy. The Grep tool passes these to ripgrep as `--glob '!<p>'`
/// excludes so a search never reads a path the policy forbids reading — whether
/// reached by a recursive walk or a `glob` arg that targets a denied file.
/// (An explicitly-passed denied `path` is blocked earlier by the permission
/// manager, since ripgrep searches explicit paths even against excludes.)
/// Empty when no managed Read denies apply.
#[derive(Debug, Clone, Default)]
pub struct DenyReadGlobs(pub Vec<String>);
/// Resolve a model-provided path, rewriting absolute paths from conversation
/// history when [`DisplayCwd`] is set.
///
/// - If `display_cwd` is `None`, falls back to `cwd.join(input)`.
/// - If `input` starts with the `display_cwd` prefix, strips it and joins
///   the suffix onto `cwd` (the real worktree path).
/// - If `input` is absolute but doesn't match, returns it as-is.
/// - Leading `~`/`~/` is expanded to the current user's home directory
///   before applying the above rules. `~username` is not expanded.
/// - Relative paths are always joined onto `cwd`.
pub fn resolve_model_path(
    cwd: &std::path::Path,
    display_cwd: Option<&std::path::Path>,
    input: &str,
) -> PathBuf {
    let input = sanitize_model_path_arg(input);
    let expanded = shellexpand::tilde_with_context(input, || {
        echo_agent_config::discovery_home_dir().map(|home| home.to_string_lossy().into_owned())
    });
    let input_path = std::path::Path::new(expanded.as_ref());
    if let Some(display) = display_cwd
        && input_path.is_absolute()
    {
        if let Ok(suffix) = input_path.strip_prefix(display) {
            return cwd.join(suffix);
        }
        return input_path.to_path_buf();
    }
    if !input_path.is_absolute() && !expanded.is_empty() {
        let as_absolute = std::path::PathBuf::from(format!("/{}", expanded.as_ref()));
        let effective_base = display_cwd.unwrap_or(cwd);
        if as_absolute.starts_with(effective_base)
            && let Ok(suffix) = as_absolute.strip_prefix(effective_base)
        {
            return cwd.join(suffix);
        }
    }
    cwd.join(input_path)
}
/// Strip surrounding whitespace (e.g. a trailing newline from block-form
/// tool args) and quotes that models occasionally emit around path args.
///
/// When the arg was quote-wrapped, the model emitted a *string literal* (e.g.
/// a JSON-style `"/path/file.ts\n"` pasted into a block-form arg where no
/// JSON unescaping ever runs). In that case also strip trailing **literal**
/// escape sequences (`\n`, `\r`, `\t` as two characters) left at the end of
/// the unquoted value — `str::trim` only removes real whitespace, so the
/// resolved path would otherwise end in a literal backslash-n and miss the
/// file. Escape stripping requires the trimmed arg to both *start and end*
/// with a quote character (true quote-wrapping): a stray unbalanced quote is
/// still stripped, but does not enable escape stripping, so backslashes in
/// otherwise-unquoted real paths (e.g. Windows `dir\n ame`) are never eaten.
fn sanitize_model_path_arg(input: &str) -> &str {
    let trimmed = input.trim();
    let quote_wrapped =
        trimmed.len() >= 2 && trimmed.starts_with(['"', '\'']) && trimmed.ends_with(['"', '\'']);
    let unquoted = trimmed.trim_matches(['"', '\'']).trim();
    if !quote_wrapped {
        return unquoted;
    }
    let mut result = unquoted;
    while let Some(stripped) = result
        .strip_suffix("\\n")
        .or_else(|| result.strip_suffix("\\r"))
        .or_else(|| result.strip_suffix("\\t"))
    {
        result = stripped.trim_end();
    }
    result
}
/// Return the display path (for model-facing output) or fall back to cwd.
pub fn display_cwd_or_cwd(cwd: &std::path::Path, display_cwd: Option<&std::path::Path>) -> PathBuf {
    display_cwd.unwrap_or(cwd).to_path_buf()
}
/// Newtype wrapper for `Arc<dyn echo_agent_tool_runtime::ToolDispatch>` so it can
/// be stored in `ToolCallContext::extensions`. Used by `use_tool` and the
/// external MCP-call tool, which dispatch to target tools without going
/// through the outer `ToolBridge` (which would deadlock).
/// Desktop-coordinated MCP writes must not replay an uncertain request.
#[derive(Clone, Copy, Debug)]
pub struct DesktopMcpNoReplay(pub bool);

#[derive(Clone)]
pub struct InnerDispatch(pub std::sync::Arc<dyn echo_agent_tool_runtime::ToolDispatch>);
#[derive(Debug, Clone)]
pub struct ManagedGatewayToolSource {
    pub connector_id: String,
    pub connector_name: String,
    pub tool_id: String,
    pub tool_name: String,
    pub call_id: String,
}
#[derive(Debug, Clone, Default)]
pub struct ManagedGatewayToolCatalog(pub HashMap<String, ManagedGatewayToolSource>);
impl ManagedGatewayToolCatalog {
    pub fn get(&self, name: &str) -> Option<&ManagedGatewayToolSource> {
        self.0.get(name)
    }
}
#[derive(Debug, Clone)]
pub struct ManagedGatewayToolCallResponse {
    pub result: serde_json::Value,
    pub connectors_needing_reauth: Vec<String>,
}
#[async_trait::async_trait]
pub trait ManagedGatewayToolCaller: Send + Sync {
    async fn call_tool(
        &self,
        call_id: &str,
        arguments: serde_json::Value,
        caller: &str,
    ) -> Result<ManagedGatewayToolCallResponse, echo_agent_tool_runtime::ToolError>;
}
#[derive(Clone)]
pub struct ManagedGatewayToolClient(pub Arc<dyn ManagedGatewayToolCaller>);
impl std::fmt::Debug for ManagedGatewayToolClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedGatewayToolClient").finish()
    }
}
/// Whether streaming output is enabled for this invocation.
#[derive(Debug, Clone, Copy)]
pub struct StreamEnabled(pub bool);
/// Client-configurable truncation settings.
#[derive(Debug, Clone)]
pub struct TruncationCfg(pub crate::types::context::TruncationConfig);
/// Environment variables from .envrc etc.
#[derive(Debug, Clone)]
pub struct SessionEnv(pub Arc<HashMap<String, String>>);
/// Whether system reminders are enabled globally.
#[derive(Debug, Clone, Copy)]
pub struct SystemRemindersEnabled(pub bool);
/// Enforces `.gitignore` patterns on file-access tools (`read_file`, `search_replace`).
///
/// Seeded at session start from the same rules used by AGENTS.md discovery.
/// When absent (no git repo), tools allow all files.
#[derive(Clone)]
pub struct GitignoreFilter {
    gitignore: ignore::gitignore::Gitignore,
    git_root: PathBuf,
}
impl GitignoreFilter {
    pub fn new(gitignore: ignore::gitignore::Gitignore, git_root: PathBuf) -> Self {
        Self {
            gitignore,
            git_root,
        }
    }
    /// Check whether a path is gitignored.
    ///
    /// For non-existent files (new file creation), canonicalizes the parent
    /// directory to handle symlinks (e.g., macOS `/var` → `/private/var`).
    pub fn is_ignored(&self, path: &std::path::Path) -> bool {
        let normalized = dunce::canonicalize(path).unwrap_or_else(|_| {
            path.parent()
                .and_then(|parent| {
                    dunce::canonicalize(parent)
                        .ok()
                        .map(|p| p.join(path.file_name().unwrap_or_default()))
                })
                .unwrap_or_else(|| path.to_path_buf())
        });
        crate::gitignore::is_ignored(&self.gitignore, &normalized, Some(&self.git_root))
    }
}
impl std::fmt::Debug for GitignoreFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitignoreFilter")
            .field("git_root", &self.git_root)
            .finish()
    }
}
/// Controls whether tools respect `.gitignore` patterns.
///
/// Always seeded by `agent_rebuild`. When `true`, all tools block gitignored
/// files. When `false`, `read_file` allows via `is_some_and` while
/// `grep`/`list_dir`/`search_replace` also allow via `is_none_or`.
///
/// Configured via `[tools] respect_gitignore = true` in `config.toml`.
#[derive(Debug, Clone, Copy)]
pub struct RespectGitignore(pub bool);
impl Default for RespectGitignore {
    fn default() -> Self {
        Self(true)
    }
}
/// Whether to enrich path-not-found errors with CWD reminders, "dropped repo
/// folder" correction, and similar-name suggestions.
///
/// Default `false`. Hosts may enable this via remote config or local settings.
#[derive(Debug, Clone, Copy, Default)]
pub struct PathNotFoundHints(pub bool);
/// Whether scheduled task fires execute in background loop subagents.
///
/// `false` forces every fire onto the legacy main-conversation path.
/// Configured via `[scheduler] background_loops` in `config.toml`, the
/// `ECHO_AGENT_SCHEDULER_BACKGROUND_LOOPS` env var, or the
/// `scheduler_background_loops` remote setting.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerBackgroundLoops(pub bool);
impl Default for SchedulerBackgroundLoops {
    fn default() -> Self {
        Self(true)
    }
}
/// Map of canonical tool names → model-facing tool names.
#[derive(Debug, Clone, Default)]
pub struct ToolNameMapping(pub HashMap<String, String>);
impl ToolNameMapping {
    /// Resolve a canonical tool name to the model-facing name.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.0
            .get(canonical)
            .map(|s| s.as_str())
            .unwrap_or(canonical)
    }
}
/// Set of client-facing names of all enabled **native** (non-MCP) tools.
///
/// Populated once at `finalize()` from the finalized tool list (every tool
/// whose client-facing name does not contain the `__` MCP delimiter). Used by
/// `use_tool` to detect when the model wrongly routes a native tool call
/// (e.g. `scheduler_create`) through `use_tool`. Without this, such calls hit
/// the generic "not a valid MCP tool name" error and the model gets stuck,
/// because `search_tool` only indexes MCP tools.
///
/// Detected at runtime by `use_tool::run()` to return a corrective error
/// ("call it directly") instead of the generic "not a valid MCP tool name"
/// message that left the model stuck.
#[derive(Debug, Clone, Default)]
pub struct EnabledNativeToolNames(pub std::collections::HashSet<String>);
impl EnabledNativeToolNames {
    /// Whether `name` is an enabled native (non-MCP) tool.
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}
/// Map of canonical tool name → {canonical param name → model-facing param name}.
#[derive(Debug, Clone, Default)]
pub struct ParamNameMapping(pub HashMap<String, HashMap<String, String>>);
impl ParamNameMapping {
    /// Resolve a canonical parameter name for a given tool.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(&'a self, tool: &str, canonical: &'a str) -> &'a str {
        self.0
            .get(tool)
            .and_then(|m| m.get(canonical))
            .map(|s| s.as_str())
            .unwrap_or(canonical)
    }
}
/// Canonical → client-facing param names for the tool currently executing.
///
/// Stamped onto [`echo_agent_tool_runtime::ToolCallContext::extensions`] by
/// `prepare_dispatch` / `call_raw` from that tool's own
/// `params_name_overrides`. Prefer this over kind-wide
/// [`crate::types::template_renderer::TemplateRenderer::param_for_kind`] when
/// naming params in that tool's own errors — multiple tools can share a
/// `ToolKind` with different renames, and the kind map is first/last-wins.
#[derive(Debug, Clone, Default)]
pub struct InvokingToolParamNames(pub HashMap<String, String>);
impl InvokingToolParamNames {
    /// Build from a client→canonical reverse map (the dispatch remap direction).
    pub fn from_reverse_params(reverse_params: &HashMap<String, String>) -> Self {
        Self(
            reverse_params
                .iter()
                .map(|(client, canonical)| (canonical.clone(), client.clone()))
                .collect(),
        )
    }
    /// Resolve a canonical parameter name for the invoking tool.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(&'a self, canonical: &'a str) -> &'a str {
        self.0
            .get(canonical)
            .map(String::as_str)
            .unwrap_or(canonical)
    }
}
/// Map of `ToolKind` → client-facing tool name.
///
/// Built at finalize time from the enabled tools and client name overrides.
/// Used at runtime by tools that reference other tools in error messages
/// (e.g., search_replace saying "use the Read tool first").
///
/// This is the **kind-based** counterpart to `ToolNameMapping`. Tools query
/// by semantic role (`ToolKind::Read`), not canonical name (`"read_file"`).
#[derive(Debug, Clone, Default)]
pub struct ToolKindNames(pub HashMap<crate::types::tool::ToolKind, String>);
/// Map of `ToolKind` → { canonical param name → client-facing param name }.
///
/// Built at finalize time from client param overrides. Used at runtime by
/// tools that reference their own (or other tools') param names in error
/// messages (e.g., "use `replaceAll` to replace all occurrences").
#[derive(Debug, Clone, Default)]
pub struct ParamKindNames(pub HashMap<crate::types::tool::ToolKind, HashMap<String, String>>);
impl ParamKindNames {
    /// Resolve a canonical parameter name for a given tool kind.
    /// Falls back to the canonical name if not in the map.
    pub fn resolve<'a>(
        &'a self,
        kind: crate::types::tool::ToolKind,
        canonical: &'a str,
    ) -> &'a str {
        self.0
            .get(&kind)
            .and_then(|m| m.get(canonical))
            .map(|s| s.as_str())
            .unwrap_or(canonical)
    }
}
/// Available skills for description template rendering.
///
/// Stored in Resources so `build_description_context()` can populate the
/// `skills` field of `DescriptionContext`. Inserted by `with_backend()`
/// before any tools are registered.
#[derive(Debug, Clone)]
pub struct AvailableSkills(pub Vec<crate::implementations::skills::types::SkillInfo>);
impl AvailableSkills {
    /// Check if a skill with the given name is available for model invocation.
    ///
    /// Returns `false` for skills with `disable_model_invocation = true` (model
    /// cannot auto-invoke) or `user_invocable = false` (not shown in skill tool),
    /// since the model would be unable to successfully invoke them.
    pub fn has_skill(&self, name: &str) -> bool {
        self.0
            .iter()
            .any(|s| s.name == name && s.enabled && !s.disable_model_invocation && s.user_invocable)
    }
}
/// Session folder for logs and output files.
#[derive(Debug, Clone)]
pub struct SessionFolder(pub PathBuf);
/// Per-turn registry mapping each attached image's `[Image #N]` display
/// number to a reference `image_edit` can resolve.
///
/// The model sees attachments inline (as pixels) and only the `[Image #N]`
/// token in text — never a path — so this lets `image_edit` resolve that
/// token instead of fabricating a filesystem path it can't know.
///
/// Keyed by display **number**, not list position: numbers are not
/// renumbered when a chip is removed mid-compose (`#1` and `#3` survive
/// after `#2`) and images may be dropped during normalization, so the two
/// diverge. Each reference is a bare filesystem path (the durable
/// `session_image_path`) or a `data:<mime>;base64,<data>` URL fallback.
///
/// Replaced wholesale each turn (empty when there are no attachments) so a
/// stale registry never resolves to a prior turn's image. Ephemeral — not
/// persisted, not serde-registered.
#[derive(Debug, Clone, Default)]
pub struct AttachedImages(pub Vec<(usize, String)>);
impl AttachedImages {
    /// Resolve an `[Image #N]` display number to its reference string.
    pub fn reference_for(&self, display_number: usize) -> Option<&str> {
        self.0
            .iter()
            .find(|(n, _)| *n == display_number)
            .map(|(_, reference)| reference.as_str())
    }
}
/// Notification handle for streaming tool output.
#[derive(Clone)]
pub struct NotificationHandle(pub ToolNotificationHandle);
impl std::fmt::Debug for NotificationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationHandle").finish()
    }
}
/// File system abstraction.
pub struct FileSystem(pub Arc<dyn AsyncFileSystem>);
impl std::fmt::Debug for FileSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSystem").finish()
    }
}
/// Terminal backend abstraction.
pub struct Terminal(pub Arc<dyn TerminalBackend>);
/// Session ID that owns processes spawned by this session's tools.
/// Used to scope kill operations so subagent teardown only kills
/// the subagent's own tasks on a shared terminal backend.
#[derive(Debug, Clone)]
pub struct OwnerSessionId(pub String);
/// Shared citation counter for `[web:N]` numbering across web tools.
///
/// Stored as `State<WebCitationCounter>` in Resources so web tools that emit
/// citations share the same monotonically increasing counter within a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebCitationCounter {
    pub counter: u32,
}
impl WebCitationCounter {
    /// Return the current value and increment.
    pub fn next_citation(&mut self) -> u32 {
        let val = self.counter;
        self.counter += 1;
        val
    }
}
register_resource!("echo_agent_build", "WebCitation", WebCitationCounter);
impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal").finish()
    }
}
/// Per-tool retry/backoff configurations.
/// Set by the agent builder, consumed by the bridge's retry loop.
///
/// NOT persisted — ephemeral runtime state that's re-set on each session.
#[derive(Debug, Clone, Default)]
pub struct ToolRetries(pub HashMap<String, crate::retry::BackoffConfig>);
impl ToolRetries {
    /// Set a retry config for a specific tool.
    pub fn set(&mut self, tool: &str, config: crate::retry::BackoffConfig) {
        self.0.insert(tool.to_string(), config);
    }
    /// Get the retry config for a specific tool, if set.
    pub fn get(&self, tool: &str) -> Option<&crate::retry::BackoffConfig> {
        self.0.get(tool)
    }
    /// Clear all retry configs.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
/// Tracks whether a required "completion" tool has been called this turn.
///
/// Used by agent definitions that require a specific tool to be called
/// before the agent can be considered "done" (e.g. a workflow's
/// `complete_task` tool).
///
/// Ephemeral — NOT persisted. Stored in Resources, not serde-registered.
#[derive(Debug, Clone)]
pub struct CompletionTracker {
    /// Canonical name of the tool that must be called.
    pub tool: String,
    /// Reminder text to inject if the tool hasn't been called.
    pub reminder: String,
    /// Whether the tool was called during the current turn.
    pub called_this_turn: bool,
}
impl ResourceType for CompletionTracker {
    const ID: &'static str = "";
}
/// Metadata for a single MCP resource, returned by [`McpResourceProvider::list_resources`].
#[derive(Debug, Clone)]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub server: String,
}
/// Content payload returned by [`McpResourceProvider::read_resource`].
#[derive(Debug)]
pub enum McpResourceContent {
    Text(String),
    Blob(Vec<u8>),
}
/// Result of reading a single MCP resource.
#[derive(Debug)]
pub struct McpResourceReadResult {
    pub uri: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub content: Option<McpResourceContent>,
}
/// Provider trait for MCP resource operations.
///
/// Injected into `SharedResources` by the shell layer so tools
/// (`ListMcpResources`, `FetchMcpResource`) can access MCP servers without
/// depending on `echo-agent-mcp` directly.  Follows the same pattern as
/// [`FileSystem`] (`Arc<dyn AsyncFileSystem>`).
#[async_trait::async_trait]
pub trait McpResourceProvider: Send + Sync {
    /// List resources from one or all MCP servers.
    async fn list_resources(&self, server: Option<String>) -> Result<Vec<McpResourceInfo>, String>;
    /// Read a specific resource by server name and URI.
    async fn read_resource(
        &self,
        server: String,
        uri: String,
    ) -> Result<McpResourceReadResult, String>;
}
/// Wrapper stored in [`Resources`] for MCP resource access.
#[derive(Clone)]
pub struct McpResourceAccess(pub Arc<dyn McpResourceProvider>);
impl std::fmt::Debug for McpResourceAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpResourceAccess").finish()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct EditConfig {
        skip_read_before_edit: bool,
        max_file_size: Option<usize>,
    }
    register_resource!("echo_agent_build", "Edit", EditConfig);
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct ReadHistory {
        files_read: Vec<String>,
    }
    register_resource!("echo_agent_build", "ReadFile", ReadHistory);
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct TodoData {
        items: Vec<String>,
    }
    register_resource!("echo_agent_build", "Todo", TodoData);
    #[test]
    fn insert_and_get_typed_values() {
        let mut res = Resources::new();
        res.insert(42u32);
        res.insert("hello".to_string());
        assert_eq!(res.get::<u32>(), Some(&42));
        assert_eq!(res.get::<String>(), Some(&"hello".to_string()));
        assert_eq!(res.get::<bool>(), None);
    }
    #[test]
    fn get_mut_modifies_in_place() {
        let mut res = Resources::new();
        res.insert(10i32);
        *res.get_mut::<i32>().unwrap() += 5;
        assert_eq!(res.get::<i32>(), Some(&15));
    }
    #[test]
    fn get_or_default_inserts_when_missing() {
        let mut res = Resources::new();
        let val = res.get_or_default::<Vec<i32>>();
        val.push(1);
        val.push(2);
        assert_eq!(res.get::<Vec<i32>>(), Some(&vec![1, 2]));
    }
    #[test]
    fn get_or_default_returns_existing() {
        let mut res = Resources::new();
        res.insert(vec![42i32]);
        let val = res.get_or_default::<Vec<i32>>();
        assert_eq!(val, &vec![42]);
    }
    #[test]
    fn remove_returns_value() {
        let mut res = Resources::new();
        res.insert("test".to_string());
        let removed = res.remove::<String>();
        assert_eq!(removed, Some("test".to_string()));
        assert_eq!(res.get::<String>(), None);
    }
    #[test]
    fn remove_returns_none_when_missing() {
        let mut res = Resources::new();
        assert_eq!(res.remove::<String>(), None);
    }
    #[test]
    fn contains_checks_presence() {
        let mut res = Resources::new();
        assert!(!res.contains::<u32>());
        res.insert(42u32);
        assert!(res.contains::<u32>());
    }
    #[test]
    fn params_and_state_coexist_without_collision() {
        let mut res = Resources::new();
        res.insert(Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: Some(100),
        }));
        res.insert(State(EditConfig {
            skip_read_before_edit: false,
            max_file_size: None,
        }));
        let params = res.get::<Params<EditConfig>>().unwrap();
        let state = res.get::<State<EditConfig>>().unwrap();
        assert!(params.skip_read_before_edit);
        assert_eq!(params.max_file_size, Some(100));
        assert!(!state.skip_read_before_edit);
        assert_eq!(state.max_file_size, None);
    }
    #[test]
    fn params_and_state_have_different_typeids() {
        assert_ne!(
            TypeId::of::<Params<EditConfig>>(),
            TypeId::of::<State<EditConfig>>()
        );
    }
    #[test]
    fn serde_roundtrip_registered_types() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        res.register_state::<ReadHistory>();
        res.register_state::<TodoData>();
        res.insert(Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: Some(1024),
        }));
        res.insert(State(ReadHistory {
            files_read: vec!["main.rs".to_string(), "lib.rs".to_string()],
        }));
        res.insert(State(TodoData {
            items: vec!["task1".to_string()],
        }));
        let json = res.serialize();
        let json_str = serde_json::to_string_pretty(&json).unwrap();
        let mut res2 = Resources::new();
        res2.register_params::<EditConfig>();
        res2.register_state::<ReadHistory>();
        res2.register_state::<TodoData>();
        let parsed: HashMap<String, HashMap<String, serde_json::Value>> =
            serde_json::from_str(&json_str).unwrap();
        res2.load_from(parsed);
        let params = res2.get::<Params<EditConfig>>().unwrap();
        assert!(params.0.skip_read_before_edit);
        assert_eq!(params.0.max_file_size, Some(1024));
        let state = res2.get::<State<ReadHistory>>().unwrap();
        assert_eq!(
            state.0.files_read,
            vec!["main.rs".to_string(), "lib.rs".to_string()]
        );
        let todo = res2.get::<State<TodoData>>().unwrap();
        assert_eq!(todo.0.items, vec!["task1".to_string()]);
    }
    #[test]
    fn ephemeral_types_silently_skipped_during_serialization() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        res.insert(State(ReadHistory {
            files_read: vec!["file.rs".to_string()],
        }));
        res.insert(Cwd(PathBuf::from("/home/user")));
        res.insert(StreamEnabled(true));
        let json = res.serialize();
        assert!(json.get("state").is_some());
        let state = json.get("state").unwrap();
        assert!(state.get("echo_agent_build.ReadFile").is_some());
        let json_str = serde_json::to_string(&json).unwrap();
        assert!(!json_str.contains("/home/user"));
    }
    #[test]
    fn load_from_populates_registered_types() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        res.register_params::<EditConfig>();
        let mut state_map = HashMap::new();
        state_map.insert(
            "echo_agent_build.ReadFile".to_string(),
            serde_json::json!({"files_read": ["loaded.rs"]}),
        );
        let mut params_map = HashMap::new();
        params_map.insert(
            "echo_agent_build.Edit".to_string(),
            serde_json::json!({"skip_read_before_edit": true, "max_file_size": 512}),
        );
        let mut data = HashMap::new();
        data.insert("state".to_string(), state_map);
        data.insert("params".to_string(), params_map);
        res.load_from(data);
        let history = res.get::<State<ReadHistory>>().unwrap();
        assert_eq!(history.0.files_read, vec!["loaded.rs".to_string()]);
        let config = res.get::<Params<EditConfig>>().unwrap();
        assert!(config.0.skip_read_before_edit);
        assert_eq!(config.0.max_file_size, Some(512));
    }
    #[test]
    fn load_from_ignores_unknown_keys() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        let mut state_map = HashMap::new();
        state_map.insert(
            "unknown.Type".to_string(),
            serde_json::json!({"foo": "bar"}),
        );
        state_map.insert(
            "echo_agent_build.ReadFile".to_string(),
            serde_json::json!({"files_read": ["ok.rs"]}),
        );
        let mut data = HashMap::new();
        data.insert("state".to_string(), state_map);
        res.load_from(data);
        let history = res.get::<State<ReadHistory>>().unwrap();
        assert_eq!(history.0.files_read, vec!["ok.rs".to_string()]);
    }
    #[test]
    fn get_json_returns_registered_value() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        res.insert(Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: None,
        }));
        let val = res.get_json("params", "echo_agent_build.Edit").unwrap();
        assert_eq!(val["skip_read_before_edit"], true);
    }
    #[test]
    fn get_json_returns_none_for_unregistered() {
        let res = Resources::new();
        assert!(res.get_json("params", "nonexistent").is_none());
    }
    #[test]
    fn get_json_returns_none_for_missing_value() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        assert!(res.get_json("params", "echo_agent_build.Edit").is_none());
    }
    #[test]
    fn set_json_updates_registered_value() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        let ok = res.set_json(
            "params",
            "echo_agent_build.Edit",
            serde_json::json!({"skip_read_before_edit": true}),
        );
        assert!(ok);
        let config = res.get::<Params<EditConfig>>().unwrap();
        assert!(config.0.skip_read_before_edit);
    }
    #[test]
    fn set_json_returns_false_for_unregistered() {
        let mut res = Resources::new();
        let ok = res.set_json("params", "unknown", serde_json::json!({}));
        assert!(!ok);
    }
    #[test]
    fn double_register_is_idempotent() {
        let mut res = Resources::new();
        res.register_state::<ReadHistory>();
        res.register_state::<ReadHistory>();
        assert_eq!(res.entries.len(), 1);
    }
    #[test]
    fn serialize_empty_resources_produces_empty_object() {
        let res = Resources::new();
        let json = res.serialize();
        assert_eq!(json, serde_json::json!({}));
    }
    #[test]
    fn serialize_with_registrations_but_no_values() {
        let mut res = Resources::new();
        res.register_params::<EditConfig>();
        res.register_state::<ReadHistory>();
        let json = res.serialize();
        assert_eq!(json, serde_json::json!({}));
    }
    #[test]
    fn tool_name_mapping_resolve() {
        let mut mapping = ToolNameMapping::default();
        mapping
            .0
            .insert("read_file".to_string(), "Read".to_string());
        assert_eq!(mapping.resolve("read_file"), "Read");
        assert_eq!(mapping.resolve("grep"), "grep");
    }
    #[test]
    fn param_name_mapping_resolve() {
        let mut mapping = ParamNameMapping::default();
        let mut tool_map = HashMap::new();
        tool_map.insert("old_string".to_string(), "find".to_string());
        mapping.0.insert("search_replace".to_string(), tool_map);
        assert_eq!(mapping.resolve("search_replace", "old_string"), "find");
        assert_eq!(
            mapping.resolve("search_replace", "new_string"),
            "new_string"
        );
        assert_eq!(mapping.resolve("other_tool", "old_string"), "old_string");
    }
    #[test]
    fn invoking_tool_param_names_from_reverse_and_resolve() {
        let reverse = HashMap::from([
            ("start_line".to_string(), "offset".to_string()),
            ("max_lines".to_string(), "limit".to_string()),
        ]);
        let names = InvokingToolParamNames::from_reverse_params(&reverse);
        assert_eq!(names.resolve("offset"), "start_line");
        assert_eq!(names.resolve("limit"), "max_lines");
        assert_eq!(names.resolve("path"), "path");
    }
    #[test]
    fn params_deref() {
        let p = Params(EditConfig {
            skip_read_before_edit: true,
            max_file_size: Some(42),
        });
        assert!(p.skip_read_before_edit);
        assert_eq!(p.max_file_size, Some(42));
    }
    #[test]
    fn state_deref_mut() {
        let mut s = State(ReadHistory { files_read: vec![] });
        s.files_read.push("new.rs".to_string());
        assert_eq!(s.files_read, vec!["new.rs".to_string()]);
    }
    #[test]
    fn resolve_model_path_relative_no_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_absolute_matching_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result =
            super::resolve_model_path(cwd, Some(display), "/home/user/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_absolute_non_matching() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "/etc/hosts");
        assert_eq!(result, std::path::PathBuf::from("/etc/hosts"));
    }
    #[test]
    fn resolve_model_path_relative_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_root_itself() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "/home/user/project");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    /// Kimi sent a bare colon as grep path. Should be treated as relative
    /// (joined onto cwd), NOT produce a worktree-path leak.
    #[test]
    fn resolve_model_path_bare_colon() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), ":");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/:"));
    }
    /// Kimi sent ":/testbed/cache/cache.go" — colon before display path.
    /// This is NOT absolute (doesn't start with '/'), so treated as relative.
    #[test]
    fn resolve_model_path_colon_prefixed_display_path() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), ":/testbed/cache/cache.go");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/:/testbed/cache/cache.go"),
        );
    }
    /// Empty string input — should resolve to cwd itself.
    #[test]
    fn resolve_model_path_empty_string() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    /// Absolute path that is a partial prefix match should NOT be rewritten.
    /// e.g., display="/testbed/cache" but input="/testbed/cacheXYZ/foo" — no match.
    #[test]
    fn resolve_model_path_partial_prefix_no_match() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "/testbed/cacheXYZ/foo");
        assert_eq!(result, std::path::PathBuf::from("/testbed/cacheXYZ/foo"));
    }
    /// Dotdot traversal in relative path — should join as-is (no normalization).
    #[test]
    fn resolve_model_path_dotdot_relative() {
        let cwd = std::path::Path::new("/worktree/abc/subdir");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "../other/file.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/subdir/../other/file.rs"),
        );
    }
    /// Absolute path matching display with trailing slash — strip_prefix
    /// handles this because Path normalizes trailing slashes.
    #[test]
    fn resolve_model_path_display_trailing_slash() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/testbed/cache");
        let result = super::resolve_model_path(cwd, Some(display), "/testbed/cache/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Trailing newline (from block-form tool args) must be stripped so the
    /// path targets `foo`, not a file literally named `foo\n`.
    #[test]
    fn resolve_model_path_trailing_newline() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "/worktree/abc/tsconfig.json\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/tsconfig.json")
        );
    }
    /// Trailing newline on a relative path is trimmed before the cwd join.
    #[test]
    fn resolve_model_path_relative_trailing_newline() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src/main.rs\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// A trailing newline must not defeat the display-cwd rewrite (the
    /// absolute prefix match would otherwise fail on `...main.rs\n`).
    #[test]
    fn resolve_model_path_trailing_newline_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result =
            super::resolve_model_path(cwd, Some(display), "/home/user/project/src/main.rs\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Leading/trailing spaces and tabs are trimmed.
    #[test]
    fn resolve_model_path_surrounding_whitespace() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "  \tsrc/main.rs \r\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Whitespace outside quotes is trimmed, then the quotes are stripped.
    #[test]
    fn resolve_model_path_quoted_with_surrounding_whitespace() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, " \"src/main.rs\"\n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Whitespace *inside* the quotes (trailing newline before the closing
    /// quote) is caught by the second trim after quote-stripping.
    #[test]
    fn resolve_model_path_newline_inside_quotes() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src/main.rs\n\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Whitespace-only input still resolves to cwd (matches empty-string case).
    #[test]
    fn resolve_model_path_whitespace_only() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "  \n");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    /// A quote-wrapped arg carrying a *literal* `\n` escape sequence (two
    /// characters, backslash + n) — a JSON string literal pasted into a
    /// block-form arg with no unescaping — must resolve to the real file,
    /// not one whose name ends in a literal backslash-n.
    #[test]
    fn resolve_model_path_quoted_literal_backslash_n() {
        let cwd = std::path::Path::new("/workspace");
        let result = super::resolve_model_path(cwd, None, "\"/workspace/src/game/data.ts\\n\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/workspace/src/game/data.ts")
        );
    }
    /// Same for single quotes and stacked escapes (`\r\n`), plus outer
    /// real whitespace.
    #[test]
    fn resolve_model_path_quoted_literal_crlf_escapes() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, " 'src/main.rs\\r\\n' \n");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// The literal-escape stripping must not defeat the display-cwd rewrite.
    #[test]
    fn resolve_model_path_quoted_literal_backslash_n_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result =
            super::resolve_model_path(cwd, Some(display), "\"/home/user/project/src/main.rs\\n\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    #[test]
    fn resolve_model_path_sensitive_edit_spellings() {
        let cwd = std::path::Path::new("/worktree/abc");
        for input in ["  /etc/hosts  ", "\"/etc/hosts\\n\"", "'/etc/hosts\\r\\t'"] {
            assert_eq!(
                super::resolve_model_path(cwd, None, input),
                std::path::PathBuf::from("/etc/hosts"),
                "{input:?}"
            );
        }
    }
    /// An *unquoted* path keeps its backslashes: `\n` there may be a real
    /// path component (e.g. a Windows-style separator + dir named `n`).
    #[test]
    fn resolve_model_path_unquoted_backslash_preserved() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src\\n");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/src\\n"));
    }
    /// A stray *trailing* quote on an otherwise-unquoted path must not
    /// enable escape stripping: the quote is stripped, but the literal
    /// backslash-n is a real path component and must survive.
    #[test]
    fn resolve_model_path_stray_trailing_quote_keeps_literal_escape() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "src\\n\"");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/src\\n"));
    }
    /// Same for a stray *leading* quote: only args that both start and end
    /// with a quote are string literals eligible for escape stripping.
    #[test]
    fn resolve_model_path_stray_leading_quote_keeps_literal_escape() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src\\n");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc/src\\n"));
    }
    /// A lone quote character satisfies both starts_with and ends_with, so
    /// without the length guard it would count as quote-wrapped. It must be
    /// treated as a stray quote: stripped, resolving to cwd like empty input.
    #[test]
    fn resolve_model_path_lone_quote() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"");
        assert_eq!(result, std::path::PathBuf::from("/worktree/abc"));
    }
    #[test]
    fn display_cwd_or_cwd_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        assert_eq!(
            super::display_cwd_or_cwd(cwd, Some(display)),
            std::path::PathBuf::from("/home/user/project"),
        );
    }
    #[test]
    fn display_cwd_or_cwd_without_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(
            super::display_cwd_or_cwd(cwd, None),
            std::path::PathBuf::from("/worktree/abc"),
        );
    }
    #[test]
    fn resolve_model_path_tilde_expands_to_home() {
        let home = echo_agent_config::discovery_home_dir().expect("test requires home_dir");
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "~/foo/bar.rs");
        assert_eq!(result, home.join("foo/bar.rs"));
    }
    #[test]
    fn resolve_model_path_tilde_alone() {
        let Some(home) = echo_agent_config::discovery_home_dir() else {
            return;
        };
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(super::resolve_model_path(cwd, None, "~"), home);
    }
    #[test]
    fn resolve_model_path_tilde_slash_only() {
        let Some(home) = echo_agent_config::discovery_home_dir() else {
            return;
        };
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(super::resolve_model_path(cwd, None, "~/"), home);
    }
    #[test]
    fn resolve_model_path_tilde_no_home_falls_back_to_literal() {
        let no_home = shellexpand::tilde_with_context("~/foo", || Option::<String>::None);
        assert_eq!(no_home.as_ref(), "~/foo");
    }
    #[test]
    fn resolve_model_path_tilde_username_not_expanded() {
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(
            super::resolve_model_path(cwd, None, "~root/foo"),
            std::path::PathBuf::from("/worktree/abc/~root/foo")
        );
    }
    #[test]
    fn resolve_model_path_tilde_not_at_start() {
        let cwd = std::path::Path::new("/worktree/abc");
        assert_eq!(
            super::resolve_model_path(cwd, None, "foo~bar"),
            std::path::PathBuf::from("/worktree/abc/foo~bar")
        );
    }
    #[test]
    fn resolve_model_path_tilde_with_display_cwd() {
        let Some(home) = echo_agent_config::discovery_home_dir() else {
            return;
        };
        let cwd = std::path::Path::new("/worktree/abc");
        let display = home.join("project");
        let result = super::resolve_model_path(cwd, Some(&display), "~/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs")
        );
    }
    /// Model gives cwd path without leading "/" — should resolve to cwd itself
    /// instead of producing a doubled path like /cwd/cwd.
    #[test]
    fn resolve_model_path_forgot_leading_slash_exact_cwd() {
        let cwd = std::path::Path::new("/data/user/workspace/repo/project");
        let result = super::resolve_model_path(cwd, None, "data/user/workspace/repo/project");
        assert_eq!(result, cwd);
    }
    /// Model gives cwd + subpath without leading "/" — should resolve correctly.
    #[test]
    fn resolve_model_path_forgot_leading_slash_subpath() {
        let cwd = std::path::Path::new("/data/user/workspace/repo/project");
        let result =
            super::resolve_model_path(cwd, None, "data/user/workspace/repo/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/data/user/workspace/repo/project/src/main.rs"),
        );
    }
    /// Same pattern but with display_cwd set — model forgets "/" on the
    /// display path.
    #[test]
    fn resolve_model_path_forgot_leading_slash_with_display() {
        let cwd = std::path::Path::new("/worktree/abc");
        let display = std::path::Path::new("/home/user/project");
        let result = super::resolve_model_path(cwd, Some(display), "home/user/project/src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs"),
        );
    }
    /// Normal relative paths that don't match the cwd prefix are unaffected.
    #[test]
    fn resolve_model_path_normal_relative_unaffected() {
        let cwd = std::path::Path::new("/data/user/workspace/repo/project");
        let result = super::resolve_model_path(cwd, None, "src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/data/user/workspace/repo/project/src/main.rs"),
        );
    }
    #[test]
    fn resolve_model_path_strips_leading_trailing_quotes() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src/main.rs\"");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs"),
        );
    }
    #[test]
    fn resolve_model_path_strips_leading_quote_only() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "\"src/main.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/src/main.rs"),
        );
    }
    #[test]
    fn resolve_model_path_interior_quotes_preserved() {
        let cwd = std::path::Path::new("/worktree/abc");
        let result = super::resolve_model_path(cwd, None, "path/with\"quote/file.rs");
        assert_eq!(
            result,
            std::path::PathBuf::from("/worktree/abc/path/with\"quote/file.rs"),
        );
    }
}
