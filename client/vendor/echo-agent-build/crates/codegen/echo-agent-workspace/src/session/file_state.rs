//! File state tracking for session rewind functionality.
//!
//! This module provides the ability to capture and restore file states at specific
//! points during a session. Each "rewind point" corresponds to a user prompt and
//! stores snapshots of all files that were read or modified during that prompt's
//! processing.
//!
//! **Path Storage**: File paths in `FileSnapshot` and `RewindPoint` are stored as
//! `FlexiblePath` which can be either a `RelPathBuf` (relative to session CWD) for
//! portability across machines, or a `PathBuf` for backwards compatibility with older
//! sessions that stored absolute paths.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::file_system::{AsyncFileSystem, AsyncFsWrapper, bytes_to_string};
// Minimal ToolContext for Phase 1 compile (duplicated to break shell cycle; fields/methods needed by rewind logic preserved for identical public API).
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: std::path::PathBuf,
    pub fs: crate::file_system::AsyncFsWrapper,
}
impl ToolContext {
    pub fn new_local_context(
        cwd: std::path::PathBuf,
        fs: crate::file_system::AsyncFsWrapper,
        _runner: std::sync::Arc<dyn std::any::Any + Send + Sync>,
    ) -> Self {
        Self { cwd, fs }
    }
}
impl Default for ToolContext {
    fn default() -> Self {
        Self {
            cwd: std::path::PathBuf::new(),
            fs: crate::file_system::AsyncFsWrapper::new(std::sync::Arc::new(
                crate::file_system::MockFs::new(std::path::PathBuf::new()),
            )),
        }
    }
}
use echo_agent_paths::{RelPathBuf, ToAbsPath};

/// A flexible path that can be either a relative path (preferred) or an absolute path
/// (for backwards compatibility with older sessions that stored absolute paths).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FlexiblePath {
    Relative(RelPathBuf),
    Absolute(PathBuf),
}

impl FlexiblePath {
    /// Create a new FlexiblePath from a RelPathBuf
    pub fn from_rel(path: RelPathBuf) -> Self {
        Self::Relative(path)
    }

    /// Get the path as a Path reference
    pub fn as_path(&self) -> &Path {
        match self {
            Self::Relative(p) => p.as_ref(),
            Self::Absolute(p) => p.as_ref(),
        }
    }

    /// Convert to an absolute path using the given root.
    /// For relative paths, joins with root. For absolute paths, returns as-is.
    pub fn to_absolute(&self, root: &Path) -> PathBuf {
        match self {
            Self::Relative(p) => p.to_absolute(root),
            Self::Absolute(p) => p.clone(),
        }
    }

    /// Try to convert this path to a relative path using the given root.
    /// If this is already a relative path, returns a clone.
    /// If this is an absolute path that starts with root, converts to relative.
    /// If this is an absolute path that doesn't start with root, returns as-is.
    pub fn try_to_relative(&self, root: &Path) -> FlexiblePath {
        match self {
            Self::Relative(p) => Self::Relative(p.clone()),
            Self::Absolute(p) => {
                // Try to convert absolute to relative
                match RelPathBuf::from_absolute(root, p) {
                    Ok(rel) => Self::Relative(rel),
                    Err(_) => Self::Absolute(p.clone()),
                }
            }
        }
    }

    /// Returns true if this is a relative path
    pub fn is_relative(&self) -> bool {
        matches!(self, Self::Relative(_))
    }

    /// Get the path as a string for serialization
    fn as_str(&self) -> &str {
        match self {
            Self::Relative(p) => p.as_str(),
            Self::Absolute(p) => p.to_str().unwrap_or(""),
        }
    }
}

impl From<RelPathBuf> for FlexiblePath {
    fn from(path: RelPathBuf) -> Self {
        Self::Relative(path)
    }
}

impl AsRef<Path> for FlexiblePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl std::fmt::Display for FlexiblePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative(p) => write!(f, "{}", p.as_str()),
            Self::Absolute(p) => write!(f, "{}", p.display()),
        }
    }
}

impl ToAbsPath for FlexiblePath {
    fn to_abs_path(&self, root: &Path) -> std::borrow::Cow<'_, Path> {
        match self {
            Self::Relative(p) => std::borrow::Cow::Owned(p.to_absolute(root)),
            Self::Absolute(p) => std::borrow::Cow::Borrowed(p.as_path()),
        }
    }
}

impl ToAbsPath for &FlexiblePath {
    fn to_abs_path(&self, root: &Path) -> std::borrow::Cow<'_, Path> {
        match self {
            FlexiblePath::Relative(p) => std::borrow::Cow::Owned(p.to_absolute(root)),
            FlexiblePath::Absolute(p) => std::borrow::Cow::Borrowed(p.as_path()),
        }
    }
}

mod flexible_path_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(path: &FlexiblePath, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(path.as_str())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<FlexiblePath, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        // Try to parse as RelPathBuf first (preferred)
        match RelPathBuf::try_from(s.clone()) {
            Ok(rel_path) => Ok(FlexiblePath::Relative(rel_path)),
            // Fall back to PathBuf for absolute paths from older sessions
            Err(_) => Ok(FlexiblePath::Absolute(PathBuf::from(s))),
        }
    }
}

mod flexible_path_map_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(
        map: &HashMap<FlexiblePath, FileSnapshot>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map_ser = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            map_ser.serialize_entry(k.as_str(), v)?;
        }
        map_ser.end()
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<FlexiblePath, FileSnapshot>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let map: HashMap<String, FileSnapshot> = HashMap::deserialize(deserializer)?;
        let mut result = HashMap::with_capacity(map.len());
        for (k, v) in map {
            // Try RelPathBuf first, fall back to PathBuf
            let path = match RelPathBuf::try_from(k.clone()) {
                Ok(rel_path) => FlexiblePath::Relative(rel_path),
                Err(_) => FlexiblePath::Absolute(PathBuf::from(k)),
            };
            result.insert(path, v);
        }
        Ok(result)
    }
}

/// A snapshot of a single file's content at a specific point in time.
///
/// `path` is stored as a `FlexiblePath` (preferably relative to session CWD for portability,
/// but may be absolute for backwards compatibility with older sessions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    /// Path to the file (relative to session CWD preferred, absolute for legacy sessions).
    #[serde(with = "flexible_path_serde")]
    pub path: FlexiblePath,
    /// The content of the file at the time of snapshot (None if file didn't exist)
    pub content: Option<String>,
    /// True only when a successful tool write supplied this exact, continuous postimage.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tool_verified: bool,
    /// When this snapshot was taken
    pub captured_at: DateTime<Utc>,
}

impl FileSnapshot {
    /// Create a new file snapshot with a relative path.
    pub fn new(path: RelPathBuf, content: Option<String>) -> Self {
        Self {
            path: FlexiblePath::Relative(path),
            content,
            tool_verified: false,
            captured_at: Utc::now(),
        }
    }

    /// Create a new file snapshot with a flexible path.
    pub fn new_flexible(path: FlexiblePath, content: Option<String>) -> Self {
        Self {
            path,
            content,
            tool_verified: false,
            captured_at: Utc::now(),
        }
    }

    /// Get the path as a Path reference.
    pub fn as_path(&self) -> &Path {
        self.path.as_path()
    }

    /// Convert the path to an absolute path using the given root.
    /// For relative paths, joins with root. For absolute paths, returns as-is.
    pub fn to_absolute_path(&self, root: &Path) -> PathBuf {
        self.path.to_absolute(root)
    }

    /// Normalize this snapshot's path to relative using the given root.
    /// If the path is absolute and starts with root, it will be converted to relative.
    /// Returns a new FileSnapshot with the normalized path.
    pub fn normalize_to_relative(&self, root: &Path) -> FileSnapshot {
        FileSnapshot {
            path: self.path.try_to_relative(root),
            content: self.content.clone(),
            tool_verified: self.tool_verified,
            captured_at: self.captured_at,
        }
    }

    /// Normalize this snapshot's path to relative in place.
    pub fn normalize_to_relative_mut(&mut self, root: &Path) {
        self.path = self.path.try_to_relative(root);
    }
}

/// A rewind point representing the state at a specific user prompt.
///
/// Contains snapshots of all files that were accessed (read or modified)
/// during the processing of that prompt.
///
/// File paths are stored as `FlexiblePath` (preferably relative for portability,
/// but may be absolute for backwards compatibility with older sessions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewindPoint {
    /// Index of the user prompt in the session (0-based)
    pub prompt_index: usize,
    /// When this rewind point was created
    pub created_at: DateTime<Utc>,
    /// File snapshots captured BEFORE any operations for this prompt.
    /// Key is the path to the file.
    #[serde(with = "flexible_path_map_serde")]
    pub file_snapshots: HashMap<FlexiblePath, FileSnapshot>,
    /// File snapshots captured AFTER all operations for this prompt completed.
    /// Used to detect external modifications (if current file != after_snapshots, something else changed it).
    #[serde(default, with = "flexible_path_map_serde")]
    pub after_snapshots: HashMap<FlexiblePath, FileSnapshot>,
}

impl RewindPoint {
    /// Create a new empty rewind point for the given prompt index
    pub fn new(prompt_index: usize) -> Self {
        Self {
            prompt_index,
            created_at: Utc::now(),
            file_snapshots: HashMap::new(),
            after_snapshots: HashMap::new(),
        }
    }

    /// Add a file snapshot to this rewind point (if not already present)
    pub fn add_snapshot(&mut self, snapshot: FileSnapshot) {
        // Only capture the first snapshot for each file (the state BEFORE any operations)
        self.file_snapshots
            .entry(snapshot.path.clone())
            .or_insert(snapshot);
    }

    /// Set the after-snapshot for a file (what the agent wrote)
    pub fn set_after_snapshot(&mut self, snapshot: FileSnapshot) {
        self.after_snapshots.insert(snapshot.path.clone(), snapshot);
    }

    /// Get the snapshot for a specific file path
    pub fn get_snapshot(&self, path: &FlexiblePath) -> Option<&FileSnapshot> {
        self.file_snapshots.get(path)
    }

    /// Get the snapshot for a specific relative file path
    pub fn get_snapshot_by_rel(&self, path: &RelPathBuf) -> Option<&FileSnapshot> {
        self.file_snapshots
            .get(&FlexiblePath::Relative(path.clone()))
    }

    /// List all file paths that have snapshots in this rewind point
    pub fn snapshot_paths(&self) -> Vec<&FlexiblePath> {
        self.file_snapshots.keys().collect()
    }

    /// Normalize all paths in this rewind point to relative using the given root.
    /// This converts any absolute paths that start with root to relative paths.
    /// Useful for ensuring portability when saving sessions.
    pub fn normalize_to_relative(&mut self, root: &Path) {
        // Normalize file_snapshots
        let old_snapshots = std::mem::take(&mut self.file_snapshots);
        for (path, mut snapshot) in old_snapshots {
            let new_path = path.try_to_relative(root);
            snapshot.path = new_path.clone();
            self.file_snapshots.insert(new_path, snapshot);
        }

        // Normalize after_snapshots
        let old_after = std::mem::take(&mut self.after_snapshots);
        for (path, mut snapshot) in old_after {
            let new_path = path.try_to_relative(root);
            snapshot.path = new_path.clone();
            self.after_snapshots.insert(new_path, snapshot);
        }
    }
}

/// Lightweight metadata for a single rewind point — what the rewind picker needs
/// (which prompts have snapshots, and when) without materializing the
/// (potentially huge) file contents. Produced by [`scan_rewind_point_metas`].
#[derive(Debug)]
pub struct RewindPointMeta {
    pub prompt_index: usize,
    pub created_at: DateTime<Utc>,
    pub num_file_snapshots: usize,
}

/// Open a `rewind_points.jsonl` for streaming. `NotFound` → `Ok(None)` (no file
/// yet); other I/O errors propagate so callers can distinguish "absent" from
/// "transiently unreadable" and avoid discarding on-disk history.
fn open_rewind_points(path: &Path) -> io::Result<Option<io::BufReader<std::fs::File>>> {
    match std::fs::File::open(path) {
        Ok(f) => Ok(Some(io::BufReader::new(f))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Stream-parse a `rewind_points.jsonl` file line-by-line (bounded memory; the
/// file can be hundreds of MB), skipping malformed lines with a `warn!`. Missing
/// file → `Ok(empty)`; a transient I/O error propagates as `Err` so callers don't
/// treat an unreadable file as empty and drop history. This is the LENIENT reader;
/// the rewrite path uses a STRICT read (see `merge_rewind_points_from`).
fn read_rewind_jsonl_lines<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Vec<T>> {
    let Some(mut reader) = open_rewind_points(path)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut line = String::new();
    while reader.read_line(&mut line)? != 0 {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            match serde_json::from_str::<T>(trimmed) {
                Ok(v) => out.push(v),
                Err(e) => tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "skipping malformed rewind_points.jsonl line"
                ),
            }
        }
        line.clear();
    }
    Ok(out)
}

/// Read all rewind points (full content) for the on-demand historical load.
fn read_rewind_points_file(path: &Path) -> io::Result<Vec<RewindPoint>> {
    read_rewind_jsonl_lines(path)
}

/// Counts the entries of a JSON map without allocating its keys or values.
struct MapEntryCount(usize);

impl<'de> Deserialize<'de> for MapEntryCount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = usize;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<usize, A::Error> {
                let mut n = 0;
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {
                    n += 1;
                }
                Ok(n)
            }
        }
        deserializer.deserialize_map(V).map(MapEntryCount)
    }
}

/// Cheaply scan `rewind_points.jsonl` for per-point metadata, streaming without
/// allocating file-content `String`s (`MapEntryCount` just counts `file_snapshots`;
/// other fields are skipped by serde). `file_snapshots` is required — mirroring
/// `RewindPoint` — so the picker rejects exactly the lines the on-rewind full load
/// would (never advertising a target that won't materialize).
fn scan_rewind_point_metas(path: &Path) -> io::Result<Vec<RewindPointMeta>> {
    #[derive(Deserialize)]
    struct MetaRow {
        prompt_index: usize,
        created_at: DateTime<Utc>,
        file_snapshots: MapEntryCount,
    }
    Ok(read_rewind_jsonl_lines::<MetaRow>(path)?
        .into_iter()
        .map(|r| RewindPointMeta {
            prompt_index: r.prompt_index,
            created_at: r.created_at,
            num_file_snapshots: r.file_snapshots.0,
        })
        .collect())
}

/// Fold rewind points at indices `>= target_index` into the point at
/// `target_index - 1` (before-snapshots keep the earliest via `or_insert`,
/// after-snapshots the latest), drop the folded points, and return the survivors.
/// `target_index == 0` clears everything (no predecessor).
///
/// Pure (no I/O), so the in-memory tracker and the disk-authoritative persistence
/// path share it and can't diverge.
pub fn merge_rewind_points_from(
    mut points: Vec<RewindPoint>,
    target_index: usize,
) -> Vec<RewindPoint> {
    if target_index == 0 {
        return Vec::new();
    }
    points.sort_by_key(|p| p.prompt_index);
    // Enforce one point per prompt_index, guarding a corrupt/legacy file with
    // duplicate-index lines (the normal append-once-per-prompt flow never hits this).
    points.dedup_by_key(|p| p.prompt_index);
    let split = points.partition_point(|p| p.prompt_index < target_index);
    // Indices >= target_index, ascending (so after-snapshots keep the latest).
    let to_merge = points.split_off(split);
    if let Some(previous) = points
        .iter_mut()
        .find(|p| p.prompt_index == target_index - 1)
    {
        // Consume `to_merge` by value — move the large file-content snapshots into
        // `previous` instead of cloning (MEMORY.md).
        for merged in to_merge {
            for (path, snapshot) in merged.file_snapshots {
                // or_insert: we own `snapshot`; earliest before-snapshot wins.
                previous.file_snapshots.entry(path).or_insert(snapshot);
            }
            for (path, snapshot) in merged.after_snapshots {
                previous.after_snapshots.insert(path, snapshot);
            }
        }
    }
    points
}

/// Tracks file states across prompts in a session for rewind functionality.
///
/// The tracker maintains a list of rewind points, one per user prompt.
/// Each rewind point captures the state of files BEFORE they are read or modified
/// during that prompt's processing.
///
/// **Lazy historical loading**: a tracker built via [`with_lazy_source`] does NOT
/// read the (potentially huge) persisted rewind points up front, so resuming a
/// session is cheap. They load on demand the first time a rewind *operation* needs
/// them (see [`ensure_historical_loaded`]). Live capture and persisting the
/// current prompt's point (`get_rewind_point`) deliberately do NOT trigger the
/// load, so "resume then keep working" stays fast; the picker uses the
/// metadata-only [`get_rewind_point_metas`].
///
/// [`with_lazy_source`]: FileStateTracker::with_lazy_source
/// [`ensure_historical_loaded`]: FileStateTracker::ensure_historical_loaded
/// [`get_rewind_point_metas`]: FileStateTracker::get_rewind_point_metas
#[derive(Debug)]
pub struct FileStateTracker {
    /// All rewind points for this session, indexed by prompt_index
    rewind_points: Arc<Mutex<HashMap<usize, RewindPoint>>>,
    /// Current prompt index being processed
    current_prompt_index: Arc<Mutex<Option<usize>>>,
    /// Deferred historical source: `Some(path)` until the points are lazily
    /// loaded (then `None`); `None` from the start without a lazy source.
    lazy_source: Arc<Mutex<Option<PathBuf>>>,
}

impl Default for FileStateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStateTracker {
    /// Create a new file state tracker
    pub fn new() -> Self {
        Self {
            rewind_points: Arc::new(Mutex::new(HashMap::new())),
            current_prompt_index: Arc::new(Mutex::new(None)),
            lazy_source: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a tracker that lazily loads its historical rewind points from
    /// `lazy_path` on first rewind access (resume path). The in-memory set starts
    /// empty and live captures win over disk on load (`or_insert`), never clobbered.
    pub fn with_lazy_source(lazy_path: PathBuf) -> Self {
        Self {
            rewind_points: Arc::new(Mutex::new(HashMap::new())),
            current_prompt_index: Arc::new(Mutex::new(None)),
            lazy_source: Arc::new(Mutex::new(Some(lazy_path))),
        }
    }

    /// Materialize the deferred historical rewind points (no-op if already loaded
    /// or no lazy source). Triggered by rewind *operations* needing full file
    /// contents; in-memory points win over disk via `or_insert`, so concurrent
    /// live captures are never lost.
    ///
    /// The `lazy_source` lock is held across the (large, blocking) read + merge:
    /// releasing it early would let a concurrent rewind observe `lazy_source ==
    /// None` mid-merge and skip/truncate historical points. The source is consumed
    /// only on a SUCCESSFUL read, so a transient error leaves it set to retry
    /// (never operating on or persisting a partial set).
    async fn ensure_historical_loaded(&self) {
        let mut source = self.lazy_source.lock().await;
        // Clone the path so we can clear `source` after a successful read.
        let Some(path) = source.clone() else {
            return; // already loaded, or never lazy
        };
        let loaded = match read_rewind_points_file(&path) {
            Ok(points) => points,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "deferred rewind-point load failed; leaving lazy source set to retry"
                );
                return;
            }
        };
        if !loaded.is_empty() {
            let mut points = self.rewind_points.lock().await;
            for p in loaded {
                points.entry(p.prompt_index).or_insert(p);
            }
        }
        // Success: consume the source so subsequent calls are no-ops.
        *source = None;
    }

    /// Start tracking a new prompt
    pub async fn begin_prompt(&self, prompt_index: usize) {
        let mut current = self.current_prompt_index.lock().await;
        *current = Some(prompt_index);

        // Create a new rewind point for this prompt if it doesn't exist
        let mut points = self.rewind_points.lock().await;
        points
            .entry(prompt_index)
            .or_insert_with(|| RewindPoint::new(prompt_index));
    }

    /// End tracking for the given prompt.
    /// This captures after-snapshots for all files that were touched during the prompt.
    ///
    /// The caller provides the explicit `prompt_index` so that end_prompt works
    /// even when begin_prompt was never received (e.g. RPC failure in proxy mode).
    pub async fn end_prompt(&self, fs: &AsyncFsWrapper, prompt_index: usize) {
        // Clear internal current-prompt tracking.
        {
            let mut current = self.current_prompt_index.lock().await;
            *current = None;
        }

        // Capture after-snapshots for all files that were touched
        let paths_to_capture: Vec<FlexiblePath> = {
            let points = self.rewind_points.lock().await;
            if let Some(point) = points.get(&prompt_index) {
                point
                    .file_snapshots
                    .keys()
                    .filter(|path| !point.after_snapshots.contains_key(*path))
                    .cloned()
                    .collect()
            } else {
                vec![]
            }
        };

        for flex_path in paths_to_capture {
            let content = match &flex_path {
                FlexiblePath::Relative(rel_path) => fs
                    .try_read_file(rel_path)
                    .await
                    .and_then(|opt| opt.map(bytes_to_string).transpose())
                    .unwrap_or(None),
                FlexiblePath::Absolute(abs_path) => {
                    fs.try_read_to_string(abs_path).await.unwrap_or(None)
                }
            };

            let snapshot = FileSnapshot::new_flexible(flex_path, content);

            let mut points = self.rewind_points.lock().await;
            if let Some(point) = points.get_mut(&prompt_index) {
                point.set_after_snapshot(snapshot);
            }
        }
    }

    /// Capture a file's current state before an operation.
    /// This should be called BEFORE reading or writing a file.
    ///
    /// `path` is the absolute path to the file. It will be converted to a `RelPathBuf`
    /// (using `cwd`) for storage. Files outside the CWD are silently skipped (they
    /// don't need rewind tracking since the agent shouldn't modify them).
    ///
    /// NOTE: This method is similar to `capture_file_state_with_fs`. They are kept
    /// separate due to type system constraints (`AsyncFileSystem` trait vs `AsyncFsWrapper`
    /// concrete type). Keep them in sync when making changes.
    pub async fn capture_file_state<F: AsyncFileSystem + ?Sized>(
        &self,
        fs: &F,
        path: &Path,
        cwd: &Path,
    ) -> Result<(), crate::file_system::FsError> {
        // Skip files outside the CWD - they don't need rewind tracking
        // (e.g., /etc/hosts, system files, files in other projects)
        let Ok(rel_path) = RelPathBuf::from_absolute(cwd, path) else {
            return Ok(());
        };

        let current = self.current_prompt_index.lock().await;
        let Some(prompt_index) = *current else {
            // Not currently processing a prompt, skip capture
            return Ok(());
        };
        drop(current); // Release lock before async operations

        // Read current file content (or None if it doesn't exist)
        let content = fs
            .try_read_file(path)
            .await?
            .map(bytes_to_string)
            .transpose()?;

        let snapshot = FileSnapshot::new(rel_path, content);

        // Add to the current rewind point
        let mut points = self.rewind_points.lock().await;
        if let Some(point) = points.get_mut(&prompt_index) {
            point.add_snapshot(snapshot);
        }

        Ok(())
    }

    /// Capture a file's current state before an operation using `AsyncFsWrapper`.
    ///
    /// This is a variant of `capture_file_state` that accepts `AsyncFsWrapper`.
    /// Files outside the CWD are silently skipped (they don't need rewind tracking).
    ///
    /// NOTE: This method is similar to `capture_file_state`. They are kept separate
    /// due to type system constraints (`AsyncFsWrapper` concrete type vs generic
    /// `AsyncFileSystem` trait). Keep them in sync when making changes.
    pub async fn capture_file_state_with_fs(
        &self,
        fs: &AsyncFsWrapper,
        path: &Path,
        cwd: &Path,
    ) -> Result<(), crate::file_system::FsError> {
        // Skip files outside the CWD - they don't need rewind tracking
        // (e.g., /etc/hosts, system files, files in other projects)
        let Ok(rel_path) = RelPathBuf::from_absolute(cwd, path) else {
            return Ok(());
        };

        let current = self.current_prompt_index.lock().await;
        let Some(prompt_index) = *current else {
            // Not currently processing a prompt, skip capture
            return Ok(());
        };
        drop(current); // Release lock before async operations

        // Read current file content (or None if it doesn't exist)
        let content = fs
            .try_read_file(path)
            .await?
            .map(bytes_to_string)
            .transpose()?;

        let snapshot = FileSnapshot::new(rel_path, content);

        // Add to the current rewind point
        let mut points = self.rewind_points.lock().await;
        if let Some(point) = points.get_mut(&prompt_index) {
            point.add_snapshot(snapshot);
        }

        Ok(())
    }

    /// Add a before-snapshot with provided content for a specific prompt.
    ///
    /// Unlike `capture_file_state`, this does NOT read from the filesystem.
    /// The caller provides the content directly (e.g., from a `FileWritten`
    /// notification that already carries `previous_content`).
    ///
    /// `path` is the absolute path. `cwd` is used for relativization.
    /// Files outside the CWD are silently skipped.
    pub async fn add_before_snapshot_for_prompt(
        &self,
        prompt_index: usize,
        path: &Path,
        cwd: &Path,
        content: Option<String>,
    ) {
        // Skip files outside the CWD
        let Ok(rel_path) = RelPathBuf::from_absolute(cwd, path) else {
            return;
        };

        let snapshot = FileSnapshot::new(rel_path, content);

        let mut points = self.rewind_points.lock().await;
        let point = points
            .entry(prompt_index)
            .or_insert_with(|| RewindPoint::new(prompt_index));
        point.add_snapshot(snapshot);
    }

    pub async fn record_tool_write(
        &self,
        prompt_index: usize,
        path: &Path,
        cwd: &Path,
        before: Option<String>,
        after: Option<String>,
    ) {
        let Ok(relative) = RelPathBuf::from_absolute(cwd, path) else {
            return;
        };
        let key = FlexiblePath::Relative(relative.clone());
        let mut points = self.rewind_points.lock().await;
        let point = points
            .entry(prompt_index)
            .or_insert_with(|| RewindPoint::new(prompt_index));
        let continuous = match point.after_snapshots.get(&key) {
            Some(previous) => previous.tool_verified && previous.content == before,
            None => point
                .file_snapshots
                .get(&key)
                .is_none_or(|previous| previous.content == before),
        };
        point.add_snapshot(FileSnapshot::new(relative.clone(), before));
        let mut after = FileSnapshot::new(relative, after);
        after.tool_verified = continuous;
        point.set_after_snapshot(after);
    }

    /// Get all rewind points (materializes the deferred historical set).
    pub async fn get_rewind_points(&self) -> Vec<RewindPoint> {
        self.ensure_historical_loaded().await;
        let points = self.rewind_points.lock().await;
        let mut result: Vec<RewindPoint> = points.values().cloned().collect();
        result.sort_by_key(|p| p.prompt_index);
        result
    }

    /// Lightweight metadata for every known rewind point, for the rewind picker.
    /// Combines in-memory points with a metadata-only scan of the lazy disk source
    /// — without materializing file contents and without consuming the source (a
    /// later rewind still does the full load). In-memory points win on conflict.
    ///
    /// Lock order mirrors [`ensure_historical_loaded`] (`lazy_source` outer,
    /// `rewind_points` inner): holding `lazy_source` across both the in-memory
    /// snapshot and the disk scan stops a concurrent rewind's take→read→merge from
    /// interleaving and making the picker miss points.
    pub async fn get_rewind_point_metas(&self) -> Vec<RewindPointMeta> {
        let source = self.lazy_source.lock().await;
        let mut metas: HashMap<usize, RewindPointMeta> = {
            let points = self.rewind_points.lock().await;
            points
                .values()
                .map(|p| {
                    (
                        p.prompt_index,
                        RewindPointMeta {
                            prompt_index: p.prompt_index,
                            created_at: p.created_at,
                            num_file_snapshots: p.file_snapshots.len(),
                        },
                    )
                })
                .collect()
        };
        if let Some(path) = source.as_ref() {
            match scan_rewind_point_metas(path) {
                Ok(scanned) => {
                    for meta in scanned {
                        metas.entry(meta.prompt_index).or_insert(meta);
                    }
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "rewind-point metadata scan failed; picker shows in-memory points only"
                ),
            }
        }
        let mut result: Vec<RewindPointMeta> = metas.into_values().collect();
        result.sort_by_key(|m| m.prompt_index);
        result
    }

    /// Get a specific rewind point by prompt index. Intentionally does NOT trigger
    /// the historical load: this is the live persistence path (a just-completed
    /// prompt's point is always in memory), so resume-then-work stays fast.
    pub async fn get_rewind_point(&self, prompt_index: usize) -> Option<RewindPoint> {
        let points = self.rewind_points.lock().await;
        points.get(&prompt_index).cloned()
    }

    /// Get the current prompt index being tracked
    pub async fn current_prompt_index(&self) -> Option<usize> {
        *self.current_prompt_index.lock().await
    }

    /// Clear all rewind points after (and including) the specified prompt index.
    /// This is used when rewinding to truncate future history.
    pub async fn truncate_from(&self, prompt_index: usize) {
        self.ensure_historical_loaded().await;
        let mut points = self.rewind_points.lock().await;
        points.retain(|&idx, _| idx < prompt_index);
    }

    /// Merge rewind points at indices >= `target_index` into the previous point
    /// (`target_index - 1`), then remove the merged points.
    ///
    /// Used by ConversationOnly rewind: the conversation is rewound but files
    /// are untouched, so the file effects of the discarded prompts must be
    /// folded into the last surviving prompt's rewind point. This ensures:
    /// - `/rewind 0` can still undo all file effects (merged into point N-1)
    /// - A new prompt at `target_index` gets a fresh rewind point with correct
    ///   before-snapshots (the current disk state)
    ///
    /// For `target_index == 0` there is no previous point to merge into, so all
    /// points are simply cleared.
    pub async fn merge_and_remove_from(&self, target_index: usize) {
        self.ensure_historical_loaded().await;
        let mut points = self.rewind_points.lock().await;
        // Move the points out (no clone), merge, then rebuild the map.
        let all: Vec<RewindPoint> = std::mem::take(&mut *points).into_values().collect();
        for p in merge_rewind_points_from(all, target_index) {
            points.insert(p.prompt_index, p);
        }
    }

    /// Get the maximum prompt index that has a rewind point
    pub async fn max_prompt_index(&self) -> Option<usize> {
        self.ensure_historical_loaded().await;
        let points = self.rewind_points.lock().await;
        points.keys().max().copied()
    }

    /// Normalize all paths in all rewind points to relative using the given root.
    /// This should be called before saving/persisting the session to ensure portability.
    pub async fn normalize_all_to_relative(&self, root: &Path) {
        self.ensure_historical_loaded().await;
        let mut points = self.rewind_points.lock().await;
        for point in points.values_mut() {
            point.normalize_to_relative(root);
        }
    }

    /// Get all rewind points, normalized to relative paths.
    /// This is useful when saving sessions to ensure all paths are portable.
    pub async fn get_rewind_points_normalized(&self, root: &Path) -> Vec<RewindPoint> {
        self.ensure_historical_loaded().await;
        let points = self.rewind_points.lock().await;
        let mut result: Vec<RewindPoint> = points
            .values()
            .map(|p| {
                let mut normalized = p.clone();
                normalized.normalize_to_relative(root);
                normalized
            })
            .collect();
        result.sort_by_key(|p| p.prompt_index);
        result
    }
}

// Canonical in echo-agent-workspace-types; re-exported for existing paths.
pub use echo_agent_workspace_types::rpc::session::{
    ConflictType, FileRewindConflict, FileRewindResponse,
};

/// Rewind files to the state before `target_prompt_index`.
///
/// Shared implementation used by both `hub_server.rs` (workspace-side)
/// and potentially `acp_session.rs` (shell-side). Performs:
/// 1. Gather earliest before-snapshot per file from points >= target
/// 2. Detect conflicts (external modifications since the agent's writes)
/// 3. Revert files to their before-snapshot state
/// 4. Truncate rewind points from the target onward
///
/// Returns a `FileRewindResponse` with revert results.
pub async fn rewind_files(
    tracker: &FileStateTracker,
    fs: &crate::file_system::AsyncFsWrapper,
    target_prompt_index: usize,
) -> FileRewindResponse {
    let all_points = tracker.get_rewind_points().await;

    let mut reverted_files = Vec::new();
    let mut clean_files = Vec::new();
    let mut conflicts = Vec::new();
    let mut had_errors = false;

    // Collect files to revert: gather earliest before-snapshot per file
    let mut files_to_revert: HashMap<FlexiblePath, Option<String>> = HashMap::new();

    for point in all_points
        .iter()
        .filter(|p| p.prompt_index >= target_prompt_index)
    {
        for (path, before_snapshot) in &point.file_snapshots {
            files_to_revert
                .entry(path.clone())
                .or_insert_with(|| before_snapshot.content.clone());
        }
    }

    // Conflict detection + revert
    for (rel_path, content) in &files_to_revert {
        let current_content = match fs.try_read_to_string(rel_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(?rel_path, ?e, "rewind: failed to read current content");
                None
            }
        };
        let after_content = all_points
            .iter()
            .rev()
            .find_map(|p| p.after_snapshots.get(rel_path))
            .and_then(|s| s.content.clone());

        if current_content == after_content {
            clean_files.push(rel_path.to_string());
        } else {
            let conflict_type = if current_content.is_none() && after_content.is_some() {
                ConflictType::DeletedExternally
            } else if current_content.is_some() && after_content.is_none() {
                ConflictType::CreatedExternally
            } else {
                ConflictType::ModifiedExternally
            };
            conflicts.push(FileRewindConflict {
                path: rel_path.to_string(),
                conflict_type,
            });
        }

        // Perform the revert — AsyncFsWrapper resolves FlexiblePath via ToAbsPath
        match content {
            Some(data) => {
                if let Err(e) = fs.write_file(rel_path, data.as_bytes()).await {
                    tracing::warn!(?rel_path, ?e, "rewind: failed to restore file");
                    had_errors = true;
                    continue;
                }
            }
            None => {
                if fs.exists(rel_path).await.unwrap_or(false)
                    && let Err(e) = fs.delete_file(rel_path).await
                {
                    tracing::warn!(?rel_path, ?e, "rewind: failed to delete file");
                    had_errors = true;
                    continue;
                }
            }
        }
        reverted_files.push(rel_path.to_string());
    }

    // Truncate rewind points from the target index onward.
    // Skip truncation when errors occurred so retry data is preserved.
    if !had_errors {
        tracker.truncate_from(target_prompt_index).await;
    }

    let error = if had_errors {
        Some("Some files could not be reverted".to_string())
    } else {
        None
    };

    FileRewindResponse {
        success: !had_errors,
        target_prompt_index,
        reverted_files,
        clean_files,
        conflicts,
        error,
    }
}

/// Handle for sending file state capture requests.
/// This is a lightweight clone-able handle that can be passed to tools.
#[derive(Clone)]
pub struct FileStateHandle {
    tracker: Arc<FileStateTracker>,
}

impl FileStateHandle {
    /// Create a new handle from a tracker
    pub fn new(tracker: Arc<FileStateTracker>) -> Self {
        Self { tracker }
    }

    /// Capture file state before an operation.
    ///
    /// `path` is the absolute path to the file. `cwd` is used to convert it to
    /// a relative path for portable storage.
    pub async fn capture<F: AsyncFileSystem + ?Sized>(
        &self,
        fs: &F,
        path: &Path,
        cwd: &Path,
    ) -> Result<(), crate::file_system::FsError> {
        self.tracker.capture_file_state(fs, path, cwd).await
    }

    /// Capture file state before an operation using `AsyncFsWrapper`.
    ///
    /// `path` is the absolute path to the file. `cwd` is used to convert it to
    /// a relative path for portable storage.
    pub async fn capture_with_fs(
        &self,
        fs: &AsyncFsWrapper,
        path: &Path,
        cwd: &Path,
    ) -> Result<(), crate::file_system::FsError> {
        self.tracker.capture_file_state_with_fs(fs, path, cwd).await
    }

    /// Get the underlying tracker
    pub fn tracker(&self) -> &Arc<FileStateTracker> {
        &self.tracker
    }
}

#[cfg(test)]
mod tests {
    use super::ToolContext; // from stub above
    use super::*;
    use crate::file_system::MockFs;
    use echo_agent_paths::AbsPathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_rewind_point_creation() {
        let tracker = FileStateTracker::new();
        let cwd = AbsPathBuf::new(PathBuf::from("/test")).unwrap();
        let fs = Arc::new(MockFs::new(cwd.to_path_buf()));
        let fs_wrapper = crate::file_system::AsyncFsWrapper::new(fs);
        let ctx = ToolContext::new_local_context(cwd.to_path_buf(), fs_wrapper, Arc::new(()));

        // Start a prompt
        tracker.begin_prompt(0).await;
        assert_eq!(tracker.current_prompt_index().await, Some(0));

        // End the prompt
        tracker.end_prompt(&ctx.fs, 0).await;
        assert_eq!(tracker.current_prompt_index().await, None);

        // Rewind point should exist
        let point = tracker.get_rewind_point(0).await;
        assert!(point.is_some());
        assert_eq!(point.unwrap().prompt_index, 0);
    }

    #[tokio::test]
    async fn tool_postimages_survive_end_of_turn_and_preserve_discontinuities() {
        let cwd = PathBuf::from("/test");
        let fs = AsyncFsWrapper::new(Arc::new(MockFs::new(cwd.clone())));
        let tracker = FileStateTracker::new();
        let path = cwd.join("report.txt");
        let deleted = cwd.join("removed.txt");
        tracker.begin_prompt(0).await;
        tracker
            .record_tool_write(0, &path, &cwd, None, Some("agent edit".into()))
            .await;
        tracker
            .record_tool_write(0, &deleted, &cwd, Some("original".into()), None)
            .await;
        tracker.end_prompt(&fs, 0).await;
        let point = tracker.get_rewind_point(0).await.unwrap();
        let saved: RewindPoint =
            serde_json::from_slice(&serde_json::to_vec(&point).unwrap()).unwrap();
        let key = FlexiblePath::Relative(RelPathBuf::from_absolute(&cwd, &path).unwrap());
        let deleted_key =
            FlexiblePath::Relative(RelPathBuf::from_absolute(&cwd, &deleted).unwrap());
        assert_eq!(
            saved.after_snapshots[&key].content.as_deref(),
            Some("agent edit")
        );
        assert!(saved.after_snapshots[&key].tool_verified);
        assert_eq!(saved.after_snapshots[&deleted_key].content, None);
        assert!(saved.after_snapshots[&deleted_key].tool_verified);

        tracker
            .record_tool_write(
                0,
                &path,
                &cwd,
                Some("user edit".into()),
                Some("second agent edit".into()),
            )
            .await;
        let point = tracker.get_rewind_point(0).await.unwrap();
        assert_eq!(point.file_snapshots[&key].content, None);
        assert!(!point.after_snapshots[&key].tool_verified);
    }

    #[tokio::test]
    async fn test_truncate_from() {
        let tracker = FileStateTracker::new();
        let cwd = AbsPathBuf::new(PathBuf::from("/test")).unwrap();
        let fs = Arc::new(MockFs::new(cwd.to_path_buf()));
        let fs_wrapper = crate::file_system::AsyncFsWrapper::new(fs);
        let ctx = ToolContext::new_local_context(cwd.to_path_buf(), fs_wrapper, Arc::new(()));

        // Create multiple rewind points
        for i in 0..5 {
            tracker.begin_prompt(i).await;
            tracker.end_prompt(&ctx.fs, i).await;
        }

        // Verify all points exist
        let points = tracker.get_rewind_points().await;
        assert_eq!(points.len(), 5);

        // Truncate from index 3
        tracker.truncate_from(3).await;

        // Should only have points 0, 1, 2
        let points = tracker.get_rewind_points().await;
        assert_eq!(points.len(), 3);
        assert!(tracker.get_rewind_point(0).await.is_some());
        assert!(tracker.get_rewind_point(1).await.is_some());
        assert!(tracker.get_rewind_point(2).await.is_some());
        assert!(tracker.get_rewind_point(3).await.is_none());
    }

    #[test]
    fn test_file_snapshot() {
        // FileSnapshot uses FlexiblePath (preferably relative) for paths
        let snapshot = FileSnapshot::new(
            RelPathBuf::new("src/file.txt").unwrap(),
            Some("content".into()),
        );

        assert_eq!(snapshot.as_path(), Path::new("src/file.txt"));
        assert_eq!(snapshot.content, Some("content".into()));
    }

    #[test]
    fn test_rewind_point_add_snapshot() {
        let mut point = RewindPoint::new(0);

        // Add the first snapshot (using relative paths)
        let snapshot1 = FileSnapshot::new(RelPathBuf::new("src/a.txt").unwrap(), Some("v1".into()));
        point.add_snapshot(snapshot1);

        // Try to add second snapshot for same file - should be ignored
        let snapshot2 = FileSnapshot::new(RelPathBuf::new("src/a.txt").unwrap(), Some("v2".into()));
        point.add_snapshot(snapshot2);

        // Should still have v1
        let retrieved = point
            .get_snapshot_by_rel(&RelPathBuf::new("src/a.txt").unwrap())
            .unwrap();
        assert_eq!(retrieved.content, Some("v1".into()));
    }

    #[test]
    fn test_flexible_path_try_to_relative() {
        let root = Path::new("/home/user/project");

        // Already relative - should stay relative
        let rel = FlexiblePath::Relative(RelPathBuf::new("src/file.txt").unwrap());
        let result = rel.try_to_relative(root);
        assert!(result.is_relative());
        assert_eq!(result.as_path(), Path::new("src/file.txt"));

        // Absolute path under root - should become relative
        let abs = FlexiblePath::Absolute(PathBuf::from("/home/user/project/src/file.txt"));
        let result = abs.try_to_relative(root);
        assert!(result.is_relative());
        assert_eq!(result.as_path(), Path::new("src/file.txt"));

        // Absolute path NOT under root - should stay absolute
        let abs_other = FlexiblePath::Absolute(PathBuf::from("/other/path/file.txt"));
        let result = abs_other.try_to_relative(root);
        assert!(!result.is_relative());
        assert_eq!(result.as_path(), Path::new("/other/path/file.txt"));
    }

    #[test]
    fn test_rewind_point_normalize_to_relative() {
        let root = Path::new("/home/user/project");
        let mut point = RewindPoint::new(0);

        // Add a snapshot with an absolute path (simulating old session data)
        let abs_snapshot = FileSnapshot::new_flexible(
            FlexiblePath::Absolute(PathBuf::from("/home/user/project/src/main.rs")),
            Some("fn main() {}".into()),
        );
        point.add_snapshot(abs_snapshot);

        // Add a snapshot with a relative path
        let rel_snapshot = FileSnapshot::new(
            RelPathBuf::new("src/lib.rs").unwrap(),
            Some("pub mod foo;".into()),
        );
        point.add_snapshot(rel_snapshot);

        // Before normalization, we have mixed paths
        assert_eq!(point.file_snapshots.len(), 2);

        // Normalize
        point.normalize_to_relative(root);

        // After normalization, all paths should be relative
        for (path, snapshot) in &point.file_snapshots {
            assert!(path.is_relative(), "Path {:?} should be relative", path);
            assert!(
                snapshot.path.is_relative(),
                "Snapshot path {:?} should be relative",
                snapshot.path
            );
        }

        // Verify we can still retrieve by relative path
        let main_snapshot = point.get_snapshot_by_rel(&RelPathBuf::new("src/main.rs").unwrap());
        assert!(main_snapshot.is_some());
        assert_eq!(main_snapshot.unwrap().content, Some("fn main() {}".into()));
    }

    #[test]
    fn test_deserialize_file_snapshot_with_absolute_path() {
        // Simulate JSON from an older session that stored absolute paths
        let json = r#"{
            "path": "/home/user/project/src/main.rs",
            "content": "fn main() {}",
            "captured_at": "2024-01-01T00:00:00Z"
        }"#;

        let snapshot: FileSnapshot = serde_json::from_str(json).unwrap();

        // Should deserialize successfully with an absolute path
        assert!(!snapshot.path.is_relative());
        assert_eq!(
            snapshot.path.as_path(),
            Path::new("/home/user/project/src/main.rs")
        );
        assert_eq!(snapshot.content, Some("fn main() {}".into()));

        // Should be able to normalize it to relative
        let root = Path::new("/home/user/project");
        let normalized = snapshot.normalize_to_relative(root);
        assert!(normalized.path.is_relative());
        assert_eq!(normalized.path.as_path(), Path::new("src/main.rs"));
    }

    #[test]
    fn test_deserialize_file_snapshot_with_relative_path() {
        // Simulate JSON from a newer session that stores relative paths
        let json = r#"{
            "path": "src/main.rs",
            "content": "fn main() {}",
            "captured_at": "2024-01-01T00:00:00Z"
        }"#;

        let snapshot: FileSnapshot = serde_json::from_str(json).unwrap();

        // Should deserialize successfully with a relative path
        assert!(snapshot.path.is_relative());
        assert_eq!(snapshot.path.as_path(), Path::new("src/main.rs"));
    }

    #[test]
    fn test_deserialize_rewind_point_with_absolute_paths() {
        // Simulate JSON from an older session with absolute paths in the hashmap keys
        let json = r#"{
            "prompt_index": 0,
            "created_at": "2024-01-01T00:00:00Z",
            "file_snapshots": {
                "/home/user/project/src/main.rs": {
                    "path": "/home/user/project/src/main.rs",
                    "content": "fn main() {}",
                    "captured_at": "2024-01-01T00:00:00Z"
                },
                "/home/user/project/src/lib.rs": {
                    "path": "/home/user/project/src/lib.rs",
                    "content": "pub mod foo;",
                    "captured_at": "2024-01-01T00:00:00Z"
                }
            },
            "after_snapshots": {}
        }"#;

        let point: RewindPoint = serde_json::from_str(json).unwrap();

        // Should deserialize successfully
        assert_eq!(point.prompt_index, 0);
        assert_eq!(point.file_snapshots.len(), 2);

        // Paths should be absolute (from old session)
        for path in point.file_snapshots.keys() {
            assert!(
                !path.is_relative(),
                "Expected absolute path, got {:?}",
                path
            );
        }

        // After normalization, paths should be relative
        let root = Path::new("/home/user/project");
        let mut normalized_point = point.clone();
        normalized_point.normalize_to_relative(root);

        for (path, snapshot) in &normalized_point.file_snapshots {
            assert!(path.is_relative(), "Expected relative path, got {:?}", path);
            assert!(
                snapshot.path.is_relative(),
                "Expected relative snapshot path, got {:?}",
                snapshot.path
            );
        }

        // Should be able to retrieve by relative path after normalization
        let main_snapshot =
            normalized_point.get_snapshot_by_rel(&RelPathBuf::new("src/main.rs").unwrap());
        assert!(main_snapshot.is_some());
        assert_eq!(main_snapshot.unwrap().content, Some("fn main() {}".into()));
    }

    #[test]
    fn test_deserialize_rewind_point_with_mixed_paths() {
        // Simulate JSON with a mix of absolute and relative paths (edge case)
        let json = r#"{
            "prompt_index": 1,
            "created_at": "2024-01-01T00:00:00Z",
            "file_snapshots": {
                "/home/user/project/src/old.rs": {
                    "path": "/home/user/project/src/old.rs",
                    "content": "// old file",
                    "captured_at": "2024-01-01T00:00:00Z"
                },
                "src/new.rs": {
                    "path": "src/new.rs",
                    "content": "// new file",
                    "captured_at": "2024-01-01T00:00:00Z"
                }
            },
            "after_snapshots": {}
        }"#;

        let point: RewindPoint = serde_json::from_str(json).unwrap();

        assert_eq!(point.file_snapshots.len(), 2);

        // Normalize
        let root = Path::new("/home/user/project");
        let mut normalized = point.clone();
        normalized.normalize_to_relative(root);

        // All should now be relative
        for path in normalized.file_snapshots.keys() {
            assert!(path.is_relative(), "Expected relative path, got {:?}", path);
        }

        // Both files should be retrievable
        assert!(
            normalized
                .get_snapshot_by_rel(&RelPathBuf::new("src/old.rs").unwrap())
                .is_some()
        );
        assert!(
            normalized
                .get_snapshot_by_rel(&RelPathBuf::new("src/new.rs").unwrap())
                .is_some()
        );
    }

    #[test]
    fn test_serialize_always_produces_string_paths() {
        // Create a snapshot with relative path
        let snapshot = FileSnapshot::new(
            RelPathBuf::new("src/file.txt").unwrap(),
            Some("content".into()),
        );

        let json = serde_json::to_string(&snapshot).unwrap();

        // The path should be serialized as a plain string
        assert!(json.contains("\"path\":\"src/file.txt\""));

        // Create with absolute path
        let abs_snapshot = FileSnapshot::new_flexible(
            FlexiblePath::Absolute(PathBuf::from("/abs/path/file.txt")),
            Some("content".into()),
        );

        let abs_json = serde_json::to_string(&abs_snapshot).unwrap();

        // Absolute path should also be serialized as a string
        assert!(abs_json.contains("\"path\":\"/abs/path/file.txt\""));
    }

    // ── Lazy historical rewind-point loading ──────────────────────────────────

    /// Build a rewind point at `idx` with the given (relative path, content) files.
    fn point_with_files(idx: usize, files: &[(&str, &str)]) -> RewindPoint {
        let mut p = RewindPoint::new(idx);
        for (path, content) in files {
            p.add_snapshot(FileSnapshot::new(
                RelPathBuf::new(path).unwrap(),
                Some((*content).to_string()),
            ));
        }
        p
    }

    /// Persist rewind points to a temp `rewind_points.jsonl` (one JSON per line).
    fn write_rewind_file(points: &[RewindPoint]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for p in points {
            writeln!(f, "{}", serde_json::to_string(p).unwrap()).unwrap();
        }
        f.flush().unwrap();
        f
    }

    /// Write raw lines (verbatim) to a temp `rewind_points.jsonl`.
    fn write_rewind_raw(body: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{body}").unwrap();
        f.flush().unwrap();
        f
    }

    #[tokio::test]
    async fn lazy_get_rewind_point_singular_does_not_load() {
        let file = write_rewind_file(&[
            point_with_files(0, &[("a.rs", "v0")]),
            point_with_files(1, &[("b.rs", "v1")]),
        ]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());

        // Singular lookup must NOT trigger the historical load (live-persist path).
        assert!(tracker.get_rewind_point(0).await.is_none());
        // Nothing materialized yet.
        assert!(tracker.get_rewind_point(1).await.is_none());

        // A plural query (a rewind operation) loads the full set.
        let points = tracker.get_rewind_points().await;
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].prompt_index, 0);
        assert_eq!(points[1].prompt_index, 1);
        // Now singular lookups see the loaded points.
        assert!(tracker.get_rewind_point(0).await.is_some());
    }

    #[tokio::test]
    async fn lazy_metas_scan_without_full_load() {
        let file = write_rewind_file(&[
            point_with_files(0, &[("a.rs", "v0"), ("b.rs", "v0b")]),
            point_with_files(1, &[("c.rs", "v1")]),
            point_with_files(2, &[]),
        ]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());

        let metas = tracker.get_rewind_point_metas().await;
        assert_eq!(metas.len(), 3);
        assert_eq!(metas[0].prompt_index, 0);
        assert_eq!(metas[0].num_file_snapshots, 2);
        assert_eq!(metas[1].num_file_snapshots, 1);
        assert_eq!(metas[2].num_file_snapshots, 0);

        // The metadata scan must NOT consume the lazy source: a later rewind
        // operation still gets the full file-content snapshots.
        assert!(tracker.get_rewind_point(0).await.is_none());
        let points = tracker.get_rewind_points().await;
        assert_eq!(points.len(), 3);
        assert_eq!(
            points[0]
                .get_snapshot_by_rel(&RelPathBuf::new("a.rs").unwrap())
                .and_then(|s| s.content.clone()),
            Some("v0".to_string())
        );
    }

    #[tokio::test]
    async fn lazy_keeps_new_points_and_loads_historical_for_rewind() {
        // Historical points 0,1 on disk; nothing in memory.
        let file = write_rewind_file(&[
            point_with_files(0, &[("a.rs", "h0")]),
            point_with_files(1, &[("b.rs", "h1")]),
        ]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());

        // A new prompt during the resumed session adds an in-memory point (no load).
        let cwd = Path::new("/repo");
        tracker
            .add_before_snapshot_for_prompt(2, Path::new("/repo/c.rs"), cwd, Some("new2".into()))
            .await;
        assert!(tracker.get_rewind_point(2).await.is_some());
        // Historical still not loaded.
        assert!(tracker.get_rewind_point(0).await.is_none());

        // Rewinding to a pre-resume prompt loads the historical set and keeps the
        // new in-memory point.
        let all = tracker.get_rewind_points().await;
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|p| p.prompt_index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        // truncate_from(1) keeps only the pre-resume prompt 0.
        tracker.truncate_from(1).await;
        let remaining = tracker.get_rewind_points().await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].prompt_index, 0);
        assert_eq!(
            remaining[0]
                .get_snapshot_by_rel(&RelPathBuf::new("a.rs").unwrap())
                .and_then(|s| s.content.clone()),
            Some("h0".to_string())
        );
    }

    #[tokio::test]
    async fn lazy_live_capture_wins_over_disk_at_conflicting_index() {
        // Disk has point 0 with content "disk".
        let file = write_rewind_file(&[point_with_files(0, &[("a.rs", "disk")])]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());

        // A LIVE capture at the same index 0 (before any historical load) adds an
        // in-memory point 0 with different content.
        let cwd = Path::new("/repo");
        tracker
            .add_before_snapshot_for_prompt(0, Path::new("/repo/a.rs"), cwd, Some("mem".into()))
            .await;

        // The on-rewind historical load must NOT clobber the in-memory point 0
        // (`or_insert` keeps the live capture).
        let points = tracker.get_rewind_points().await;
        assert_eq!(points.len(), 1);
        assert_eq!(
            points[0]
                .get_snapshot_by_rel(&RelPathBuf::new("a.rs").unwrap())
                .and_then(|s| s.content.clone()),
            Some("mem".to_string())
        );
    }

    #[tokio::test]
    async fn lazy_metas_combine_memory_and_disk() {
        let file = write_rewind_file(&[point_with_files(0, &[("a.rs", "h0")])]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());

        // New in-memory point at index 1.
        let cwd = Path::new("/repo");
        tracker
            .add_before_snapshot_for_prompt(1, Path::new("/repo/b.rs"), cwd, Some("new".into()))
            .await;

        let metas = tracker.get_rewind_point_metas().await;
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].prompt_index, 0); // from disk
        assert_eq!(metas[0].num_file_snapshots, 1);
        assert_eq!(metas[1].prompt_index, 1); // from memory
        assert_eq!(metas[1].num_file_snapshots, 1);
    }

    #[tokio::test]
    async fn lazy_missing_file_is_empty_not_error() {
        let tracker =
            FileStateTracker::with_lazy_source(PathBuf::from("/nonexistent/rewind_points.jsonl"));
        assert!(tracker.get_rewind_points().await.is_empty());
        assert!(tracker.get_rewind_point_metas().await.is_empty());
    }

    #[tokio::test]
    async fn lazy_merge_and_remove_loads_historical() {
        // ConversationOnly rewind path: merge_and_remove_from must see history.
        let file = write_rewind_file(&[
            point_with_files(0, &[("a.rs", "h0")]),
            point_with_files(1, &[("b.rs", "h1")]),
            point_with_files(2, &[("c.rs", "h2")]),
        ]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());

        // Merge points >= 1 into point 0's predecessor (index 0).
        tracker.merge_and_remove_from(1).await;
        let points = tracker.get_rewind_points().await;
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].prompt_index, 0);
        // Point 0 should now also carry the merged files from points 1 and 2.
        assert!(
            points[0]
                .get_snapshot_by_rel(&RelPathBuf::new("b.rs").unwrap())
                .is_some()
        );
        assert!(
            points[0]
                .get_snapshot_by_rel(&RelPathBuf::new("c.rs").unwrap())
                .is_some()
        );
    }

    /// `get_rewind_points_normalized` is a rewind op and must trigger the
    /// historical load.
    #[tokio::test]
    async fn lazy_get_rewind_points_normalized_loads_historical() {
        let file = write_rewind_file(&[point_with_files(0, &[("a.rs", "h0")])]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());
        let normalized = tracker
            .get_rewind_points_normalized(Path::new("/repo"))
            .await;
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].prompt_index, 0);
    }

    /// `max_prompt_index` is a rewind op and must trigger the load.
    #[tokio::test]
    async fn lazy_max_prompt_index_loads_historical() {
        let file = write_rewind_file(&[point_with_files(0, &[]), point_with_files(4, &[])]);
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());
        assert_eq!(tracker.max_prompt_index().await, Some(4));
    }

    /// Concurrent live capture + rewind query: must not deadlock, and the full set
    /// after both complete must contain every point.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lazy_concurrent_capture_and_rewind() {
        let file = write_rewind_file(&[
            point_with_files(0, &[("a.rs", "h0")]),
            point_with_files(1, &[("b.rs", "h1")]),
        ]);
        let tracker = Arc::new(FileStateTracker::with_lazy_source(
            file.path().to_path_buf(),
        ));

        let t1 = tracker.clone();
        let capture = async move {
            let cwd = PathBuf::from("/repo");
            t1.add_before_snapshot_for_prompt(2, &cwd.join("c.rs"), &cwd, Some("new".into()))
                .await;
        };
        let t2 = tracker.clone();
        let query = async move { t2.get_rewind_points().await };
        let (_, points) = tokio::join!(capture, query);

        // The historical set is always visible to the query.
        assert!(points.iter().any(|p| p.prompt_index == 0));
        assert!(points.iter().any(|p| p.prompt_index == 1));

        // After both complete, every point (historical + live) is present.
        let final_all = tracker.get_rewind_points().await;
        assert_eq!(
            final_all.iter().map(|p| p.prompt_index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn scan_rewind_point_metas_reads_counts() {
        let file = write_rewind_file(&[
            point_with_files(0, &[("a.rs", "x"), ("b.rs", "y")]),
            point_with_files(5, &[("c.rs", "z")]),
        ]);
        let metas = scan_rewind_point_metas(file.path()).unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].prompt_index, 0);
        assert_eq!(metas[0].num_file_snapshots, 2);
        assert_eq!(metas[1].prompt_index, 5);
        assert_eq!(metas[1].num_file_snapshots, 1);
    }

    // ── pure merge_rewind_points_from branch coverage ────────────────────────

    #[test]
    fn merge_pure_target_zero_clears_all() {
        let pts = vec![
            point_with_files(0, &[("a.rs", "0")]),
            point_with_files(1, &[("b.rs", "1")]),
        ];
        assert!(merge_rewind_points_from(pts, 0).is_empty());
    }

    #[test]
    fn merge_pure_folds_before_or_insert_and_after_latest_wins() {
        // shared.rs touched by both points; only1.rs only by p1.
        let mut p0 = RewindPoint::new(0);
        p0.add_snapshot(FileSnapshot::new(
            RelPathBuf::new("shared.rs").unwrap(),
            Some("p0-before".into()),
        ));
        p0.set_after_snapshot(FileSnapshot::new(
            RelPathBuf::new("shared.rs").unwrap(),
            Some("p0-after".into()),
        ));
        let mut p1 = RewindPoint::new(1);
        p1.add_snapshot(FileSnapshot::new(
            RelPathBuf::new("shared.rs").unwrap(),
            Some("p1-before".into()),
        ));
        p1.add_snapshot(FileSnapshot::new(
            RelPathBuf::new("only1.rs").unwrap(),
            Some("p1-only".into()),
        ));
        p1.set_after_snapshot(FileSnapshot::new(
            RelPathBuf::new("shared.rs").unwrap(),
            Some("p1-after".into()),
        ));

        let merged = merge_rewind_points_from(vec![p0, p1], 1);
        assert_eq!(merged.len(), 1);
        let m0 = &merged[0];
        assert_eq!(m0.prompt_index, 0);
        // before-snapshot: earliest (p0) wins for shared.rs (or_insert keeps it).
        assert_eq!(
            m0.get_snapshot_by_rel(&RelPathBuf::new("shared.rs").unwrap())
                .unwrap()
                .content,
            Some("p0-before".into())
        );
        // p1's only1.rs before-snapshot is folded in.
        assert!(
            m0.get_snapshot_by_rel(&RelPathBuf::new("only1.rs").unwrap())
                .is_some()
        );
        // after-snapshot: latest (p1) wins for shared.rs (insert overwrites).
        let after_key = FlexiblePath::Relative(RelPathBuf::new("shared.rs").unwrap());
        assert_eq!(
            m0.after_snapshots.get(&after_key).unwrap().content,
            Some("p1-after".into())
        );
    }

    #[test]
    fn merge_pure_missing_predecessor_drops_merged_effects() {
        // points [0, 3], target 3 → predecessor index 2 is absent (gap), so the
        // merged point 3's file effects are dropped (matches the original).
        let merged = merge_rewind_points_from(
            vec![
                point_with_files(0, &[("a.rs", "0")]),
                point_with_files(3, &[("b.rs", "3")]),
            ],
            3,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].prompt_index, 0);
        assert!(
            merged[0]
                .get_snapshot_by_rel(&RelPathBuf::new("b.rs").unwrap())
                .is_none()
        );
    }

    #[test]
    fn merge_pure_dedups_duplicate_indices() {
        // Two lines with the same prompt_index (corrupt/legacy) collapse to one.
        let merged = merge_rewind_points_from(
            vec![
                point_with_files(0, &[("a.rs", "first")]),
                point_with_files(0, &[("a.rs", "second")]),
            ],
            5,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].prompt_index, 0);
    }

    /// Blank/whitespace and malformed lines are skipped; both readers (full load +
    /// meta scan) recover exactly the valid points.
    #[tokio::test]
    async fn readers_recover_from_blank_and_malformed_lines() {
        let p0 = serde_json::to_string(&point_with_files(0, &[("a.rs", "v0")])).unwrap();
        let p2 = serde_json::to_string(&point_with_files(2, &[("c.rs", "v2")])).unwrap();
        let file = write_rewind_raw(&format!("\n   \n{p0}\ngarbage{{not json\n{p2}\n"));

        let full = read_rewind_points_file(file.path()).unwrap();
        assert_eq!(
            full.iter().map(|p| p.prompt_index).collect::<Vec<_>>(),
            vec![0, 2]
        );
        let metas = scan_rewind_point_metas(file.path()).unwrap();
        assert_eq!(
            metas.iter().map(|m| m.prompt_index).collect::<Vec<_>>(),
            vec![0, 2]
        );

        // Same via the tracker's lazy load.
        let tracker = FileStateTracker::with_lazy_source(file.path().to_path_buf());
        let points = tracker.get_rewind_points().await;
        assert_eq!(
            points.iter().map(|p| p.prompt_index).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    /// A zero-byte file (distinct from a missing file) is `Ok(empty)`.
    #[test]
    fn readers_handle_zero_byte_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert!(read_rewind_points_file(file.path()).unwrap().is_empty());
        assert!(scan_rewind_point_metas(file.path()).unwrap().is_empty());
    }

    /// Missing → `Ok(empty)` (fresh session), but a real I/O error (here: a
    /// directory) → `Err`, so the caller keeps the lazy source set rather than
    /// treating it as empty.
    #[test]
    fn readers_distinguish_missing_from_io_error() {
        let missing = PathBuf::from("/nonexistent/dir/rewind_points.jsonl");
        assert!(read_rewind_points_file(&missing).unwrap().is_empty());
        assert!(scan_rewind_point_metas(&missing).unwrap().is_empty());

        let dir = tempfile::tempdir().unwrap();
        assert!(read_rewind_points_file(dir.path()).is_err());
        assert!(scan_rewind_point_metas(dir.path()).is_err());
    }
}
