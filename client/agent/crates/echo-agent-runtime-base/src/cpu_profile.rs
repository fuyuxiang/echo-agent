use std::fs;
// OpenOptions is only used by the Unix-only profiler implementation.
#[cfg(unix)]
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::watch;

use crate::util::echo_agent_home::echo_agent_home;

const DEFAULT_PROFILE_DIR: &str = "profiles";
const DEFAULT_FREQUENCY_HZ: i32 = 1000;
const MAX_FREQUENCY_HZ: i32 = 4000;
const MIN_FREQUENCY_HZ: i32 = 1;
const AUTO_PATH_RETRY_LIMIT: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileArtifactFormat {
    /// Legacy: kept so new clients can still decode adverts from old leaders.
    /// New binaries no longer produce SVG (that required inferno, CDDL-1.0).
    Svg,
    /// Folded stacks (`thread;frame;… count` per line). Not advertised yet —
    /// see `platform::profile_formats()` for the two-phase wire migration.
    Folded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlErrorCode {
    RuntimeProfilingUnsupported,
    ProfileAlreadyActive,
    ProfileNotActive,
    ProfileStopInProgress,
    InvalidFrequency,
    OutputPathCollision,
    ArtifactWriteFailed,
    InternalError,
}

impl std::fmt::Display for ControlErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = match self {
            Self::RuntimeProfilingUnsupported => "runtime_profiling_unsupported",
            Self::ProfileAlreadyActive => "profile_already_active",
            Self::ProfileNotActive => "profile_not_active",
            Self::ProfileStopInProgress => "profile_stop_in_progress",
            Self::InvalidFrequency => "invalid_frequency",
            Self::OutputPathCollision => "output_path_collision",
            Self::ArtifactWriteFailed => "artifact_write_failed",
            Self::InternalError => "internal_error",
        };
        f.write_str(code)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{message}")]
pub struct ControlError {
    pub code: ControlErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ControlError {
    fn new(code: ControlErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    fn with_details(
        code: ControlErrorCode,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details: Some(details),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuProfileStartOptions {
    pub output: Option<PathBuf>,
    pub frequency_hz: Option<i32>,
}

impl Default for CpuProfileStartOptions {
    fn default() -> Self {
        Self {
            output: None,
            frequency_hz: Some(DEFAULT_FREQUENCY_HZ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CpuProfileStatus {
    Inactive,
    Active {
        started_at: String,
        svg_path: PathBuf,
        frequency_hz: i32,
    },
    Stopping {
        started_at: String,
        svg_path: PathBuf,
        frequency_hz: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuProfileStopResult {
    pub svg_path: PathBuf,
    pub started_at: String,
    pub stopped_at: String,
}

pub trait ProfilerEngine: std::fmt::Debug + Send + Sync {
    fn stop(self: Box<Self>) -> Result<(), ControlError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSvgPath {
    path: PathBuf,
}

#[derive(Debug)]
struct ActiveCpuProfile {
    started_at: String,
    frequency_hz: i32,
    svg_path: PathBuf,
    engine: Box<dyn ProfilerEngine>,
}

#[derive(Debug)]
struct StoppingCpuProfile {
    started_at: String,
    frequency_hz: i32,
    svg_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownStopDisposition {
    AlreadyStopping,
    StartedShutdownStop,
}

#[derive(Debug)]
pub struct CpuProfileStopHandle {
    active: ActiveCpuProfile,
}

impl CpuProfileStopHandle {
    pub fn finish(self) -> Result<CpuProfileStopResult, ControlError> {
        let stopped_at = now_timestamp();
        self.active.engine.stop()?;

        Ok(CpuProfileStopResult {
            svg_path: self.active.svg_path,
            started_at: self.active.started_at,
            stopped_at,
        })
    }
}

#[derive(Debug)]
pub struct CpuProfileManager {
    active: Option<ActiveCpuProfile>,
    stopping: Option<StoppingCpuProfile>,
    stop_completion_tx: watch::Sender<bool>,
    _stop_completion_guard: watch::Receiver<bool>,
    /// When true, forces all capability queries to report unsupported regardless
    /// of the actual platform. Used in tests to exercise the unsupported-build
    /// code path deterministically on any host platform.
    force_unsupported: bool,
}

impl Default for CpuProfileManager {
    fn default() -> Self {
        let (stop_completion_tx, stop_completion_guard) = watch::channel(true);
        Self {
            active: None,
            stopping: None,
            stop_completion_tx,
            _stop_completion_guard: stop_completion_guard,
            force_unsupported: false,
        }
    }
}

impl CpuProfileManager {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn force_unsupported_for_test(&mut self) {
        self.force_unsupported = true;
    }

    pub fn profiling_compiled_in(&self) -> bool {
        if self.force_unsupported {
            return false;
        }
        platform::profiling_compiled_in()
    }

    pub fn runtime_cpu_profile(&self) -> bool {
        if self.force_unsupported {
            return false;
        }
        platform::runtime_cpu_profile_supported()
    }

    pub fn profile_formats(&self) -> &[ProfileArtifactFormat] {
        if self.force_unsupported {
            return &[];
        }
        platform::profile_formats()
    }

    pub fn start(
        &mut self,
        options: CpuProfileStartOptions,
    ) -> Result<CpuProfileStatus, ControlError> {
        if !self.runtime_cpu_profile() {
            return Err(ControlError::new(
                ControlErrorCode::RuntimeProfilingUnsupported,
                "runtime CPU profiling is not supported in this build",
            ));
        }
        if self.active.is_some() {
            return Err(ControlError::new(
                ControlErrorCode::ProfileAlreadyActive,
                "CPU profile is already active",
            ));
        }
        if self.stopping.is_some() {
            return Err(ControlError::new(
                ControlErrorCode::ProfileStopInProgress,
                "CPU profile stop is still in progress",
            ));
        }

        let frequency_hz = validate_frequency(options.frequency_hz)?;
        let started_at = now_timestamp();
        let resolved_path = resolve_svg_path(options.output.as_deref(), &started_at)?;
        let engine = platform::start_profiler(frequency_hz, &resolved_path.path)?;

        let status = CpuProfileStatus::Active {
            started_at: started_at.clone(),
            svg_path: resolved_path.path.clone(),
            frequency_hz,
        };

        self.active = Some(ActiveCpuProfile {
            started_at,
            frequency_hz,
            svg_path: resolved_path.path,
            engine,
        });

        Ok(status)
    }

    pub fn stop(&mut self) -> Result<CpuProfileStopResult, ControlError> {
        let stop_handle = self.take_stop_handle()?;
        let result = stop_handle.finish();
        self.complete_stop();
        result
    }

    pub fn take_stop_handle(&mut self) -> Result<CpuProfileStopHandle, ControlError> {
        if self.stopping.is_some() {
            return Err(ControlError::new(
                ControlErrorCode::ProfileStopInProgress,
                "CPU profile stop is already in progress",
            ));
        }

        let active = self.active.take().ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::ProfileNotActive,
                "CPU profile is not active",
            )
        })?;

        self.stopping = Some(StoppingCpuProfile {
            started_at: active.started_at.clone(),
            frequency_hz: active.frequency_hz,
            svg_path: active.svg_path.clone(),
        });
        let _ = self.stop_completion_tx.send(false);

        Ok(CpuProfileStopHandle { active })
    }

    pub fn complete_stop(&mut self) {
        self.stopping = None;
        let _ = self.stop_completion_tx.send(true);
    }

    pub fn subscribe_stop_completion(&self) -> watch::Receiver<bool> {
        self.stop_completion_tx.subscribe()
    }

    pub fn status(&self) -> CpuProfileStatus {
        if let Some(active) = self.active.as_ref() {
            return CpuProfileStatus::Active {
                started_at: active.started_at.clone(),
                svg_path: active.svg_path.clone(),
                frequency_hz: active.frequency_hz,
            };
        }

        if let Some(stopping) = self.stopping.as_ref() {
            return CpuProfileStatus::Stopping {
                started_at: stopping.started_at.clone(),
                svg_path: stopping.svg_path.clone(),
                frequency_hz: stopping.frequency_hz,
            };
        }

        CpuProfileStatus::Inactive
    }

    /// Finalize an active CPU profile synchronously during shutdown.
    ///
    /// This is a local convenience helper for direct manager callers. It only
    /// finalizes a currently active profile owned by this caller. If a stop is
    /// already in progress, this returns `Ok(None)` and does not coordinate with
    /// that in-flight stop. Callers that need process-wide shutdown coordination
    /// must separately wait for stop completion.
    pub fn finalize_on_shutdown(&mut self) -> Result<Option<CpuProfileStopResult>, ControlError> {
        let stop_handle = self.take_shutdown_stop_handle()?;
        match stop_handle {
            Some(stop_handle) => {
                let result = stop_handle.finish();
                self.complete_stop();
                result.map(Some)
            }
            None => Ok(None),
        }
    }

    pub fn take_shutdown_stop_handle(
        &mut self,
    ) -> Result<Option<CpuProfileStopHandle>, ControlError> {
        if self.stopping.is_some() {
            return Ok(None);
        }

        if self.active.is_none() {
            return Ok(None);
        }

        self.take_stop_handle().map(Some)
    }

    pub fn shutdown_stop_disposition(&self) -> Option<ShutdownStopDisposition> {
        if self.stopping.is_some() {
            Some(ShutdownStopDisposition::AlreadyStopping)
        } else if self.active.is_some() {
            Some(ShutdownStopDisposition::StartedShutdownStop)
        } else {
            None
        }
    }
}

fn validate_frequency(frequency_hz: Option<i32>) -> Result<i32, ControlError> {
    let frequency_hz = frequency_hz.unwrap_or(DEFAULT_FREQUENCY_HZ);
    if !(MIN_FREQUENCY_HZ..=MAX_FREQUENCY_HZ).contains(&frequency_hz) {
        return Err(ControlError::with_details(
            ControlErrorCode::InvalidFrequency,
            format!(
                "CPU profile frequency must be between {} and {} Hz",
                MIN_FREQUENCY_HZ, MAX_FREQUENCY_HZ
            ),
            serde_json::json!({
                "provided": frequency_hz,
                "min": MIN_FREQUENCY_HZ,
                "max": MAX_FREQUENCY_HZ,
            }),
        ));
    }
    Ok(frequency_hz)
}

fn resolve_svg_path(
    output: Option<&Path>,
    started_at: &str,
) -> Result<ResolvedSvgPath, ControlError> {
    let path = match output {
        Some(output) => derive_output_path(output, started_at)?,
        None => derive_default_svg_path(started_at)?,
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ControlError::with_details(
                ControlErrorCode::ArtifactWriteFailed,
                format!("failed to create profile output directory {}", parent.display()),
                serde_json::json!({ "path": parent.display().to_string(), "error": err.to_string() }),
            )
        })?;
    }

    if path.exists() {
        return Err(ControlError::with_details(
            ControlErrorCode::OutputPathCollision,
            format!("profile output path already exists: {}", path.display()),
            serde_json::json!({ "path": path.display().to_string() }),
        ));
    }

    Ok(ResolvedSvgPath { path })
}

fn derive_output_path(output: &Path, started_at: &str) -> Result<PathBuf, ControlError> {
    // Honor explicit artifact paths (`.folded`/`.txt`). An explicit `.svg`
    // path — from old client invocations or muscle memory — keeps its
    // location but is redirected to `.folded`: the artifact is folded stacks
    // now, and writing text into an `.svg`-named file would just corrupt it.
    if output
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("folded") || ext.eq_ignore_ascii_case("txt"))
    {
        return Ok(output.to_path_buf());
    }
    if output
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        return Ok(output.with_extension("folded"));
    }

    if is_directory_target(output) {
        return derive_unique_svg_path(output, "leader", started_at);
    }

    let base_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("profile");
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    derive_unique_svg_path(parent, base_name, started_at)
}

fn is_directory_target(path: &Path) -> bool {
    path.is_dir()
        || path
            .as_os_str()
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR)
}

fn derive_default_svg_path(started_at: &str) -> Result<PathBuf, ControlError> {
    derive_unique_svg_path(
        &echo_agent_home().join(DEFAULT_PROFILE_DIR),
        "leader",
        started_at,
    )
}

fn derive_unique_svg_path(
    directory: &Path,
    base_name: &str,
    started_at: &str,
) -> Result<PathBuf, ControlError> {
    let normalized_base = if base_name.is_empty() {
        "leader"
    } else {
        base_name
    };
    for attempt in 0..AUTO_PATH_RETRY_LIMIT {
        let candidate = directory.join(candidate_filename(normalized_base, started_at, attempt));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(ControlError::with_details(
        ControlErrorCode::OutputPathCollision,
        format!(
            "failed to reserve a unique auto-generated profile path in {}",
            directory.display()
        ),
        serde_json::json!({
            "directory": directory.display().to_string(),
            "base_name": normalized_base,
            "started_at": started_at,
            "retry_limit": AUTO_PATH_RETRY_LIMIT,
        }),
    ))
}

fn candidate_filename(base_name: &str, started_at: &str, attempt: u32) -> String {
    let pid = std::process::id();
    if attempt == 0 {
        format!("{}-{}-{}.folded", base_name, pid, started_at)
    } else {
        format!("{}-{}-{}-{:02}.folded", base_name, pid, started_at, attempt)
    }
}

fn now_timestamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%S%.6fZ").to_string()
}

// Module-level (not inside `mod tests`) so downstream crates' test targets
// can reach it in test-only builds.
#[cfg(any(test, feature = "test-support"))]
impl CpuProfileManager {
    pub fn start_with_engine_for_test(
        &mut self,
        options: CpuProfileStartOptions,
        engine: Box<dyn ProfilerEngine>,
    ) -> Result<CpuProfileStatus, ControlError> {
        if self.active.is_some() {
            return Err(ControlError::new(
                ControlErrorCode::ProfileAlreadyActive,
                "CPU profile is already active",
            ));
        }
        if self.stopping.is_some() {
            return Err(ControlError::new(
                ControlErrorCode::ProfileStopInProgress,
                "CPU profile stop is still in progress",
            ));
        }

        let frequency_hz = validate_frequency(options.frequency_hz)?;
        let started_at = now_timestamp();
        let resolved_path = resolve_svg_path(options.output.as_deref(), &started_at)?;
        self.active = Some(ActiveCpuProfile {
            started_at: started_at.clone(),
            frequency_hz,
            svg_path: resolved_path.path.clone(),
            engine,
        });

        Ok(CpuProfileStatus::Active {
            started_at,
            svg_path: resolved_path.path,
            frequency_hz,
        })
    }
}

#[cfg(unix)]
mod platform {
    use std::fmt::Write as _;
    use std::io::Write as _;

    use super::*;

    struct PprofProfilerEngine {
        guard: pprof::ProfilerGuard<'static>,
        // Named `svg_path` historically; now points at a `.folded` artifact.
        // The wire protocol keeps the `svg_path` field name for compat.
        svg_path: PathBuf,
    }

    impl std::fmt::Debug for PprofProfilerEngine {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PprofProfilerEngine")
                .field("svg_path", &self.svg_path)
                .finish_non_exhaustive()
        }
    }

    /// Serialize a pprof report as folded stacks: one
    /// `thread;frame;frame;… count` line per unique stack — the same format
    /// pprof's `flamegraph` feature feeds to inferno. Emitting it ourselves
    /// keeps inferno (CDDL-1.0) out of shipped binaries; render externally
    /// with speedscope.app, `inferno-flamegraph`, or flamegraph.pl.
    fn folded_stacks(report: &pprof::Report) -> String {
        let mut lines: Vec<String> = report
            .data
            .iter()
            .map(|(frames, count)| {
                let mut line = frames.thread_name_or_id();
                line.push(';');
                for frame in frames.frames.iter().rev() {
                    for symbol in frame.iter().rev() {
                        let _ = write!(&mut line, "{};", symbol);
                    }
                }
                line.pop();
                let _ = write!(&mut line, " {}", count);
                line
            })
            .collect();
        // Deterministic output: HashMap iteration order is arbitrary.
        lines.sort_unstable();
        let mut folded = lines.join("\n");
        if !folded.is_empty() {
            folded.push('\n');
        }
        folded
    }

    impl ProfilerEngine for PprofProfilerEngine {
        fn stop(self: Box<Self>) -> Result<(), ControlError> {
            let report = self.guard.report().build().map_err(|err| {
                ControlError::with_details(
                    ControlErrorCode::InternalError,
                    "failed to build CPU profile report",
                    serde_json::json!({ "error": err.to_string() }),
                )
            })?;

            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.svg_path)
                .map_err(|err| {
                    let code = if err.kind() == std::io::ErrorKind::AlreadyExists {
                        ControlErrorCode::OutputPathCollision
                    } else {
                        ControlErrorCode::ArtifactWriteFailed
                    };
                    ControlError::with_details(
                        code,
                        format!("failed to create profile {}", self.svg_path.display()),
                        serde_json::json!({ "path": self.svg_path.display().to_string(), "error": err.to_string() }),
                    )
                })?;

            file.write_all(folded_stacks(&report).as_bytes())
                .map_err(|err| {
                    ControlError::with_details(
                        ControlErrorCode::ArtifactWriteFailed,
                        format!("failed to write profile {}", self.svg_path.display()),
                        serde_json::json!({ "path": self.svg_path.display().to_string(), "error": err.to_string() }),
                    )
                })
        }
    }

    pub(super) fn profiling_compiled_in() -> bool {
        true
    }

    pub(super) fn runtime_cpu_profile_supported() -> bool {
        cfg!(unix)
    }

    pub(super) fn profile_formats() -> &'static [ProfileArtifactFormat] {
        // Advertise nothing for now: old clients deserialize this enum
        // strictly inside the Registered handshake, so a new variant (e.g.
        // `folded`) would break their connect entirely. Start advertising
        // `Folded` once binaries that know the variant have saturated the
        // fleet. The artifact itself is already folded stacks.
        &[]
    }

    pub(super) fn start_profiler(
        frequency_hz: i32,
        svg_path: &Path,
    ) -> Result<Box<dyn ProfilerEngine>, ControlError> {
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(frequency_hz)
            .blocklist(&["libc", "libgcc", "pthread", "vdso"])
            .build()
            .map_err(|err| {
                ControlError::with_details(
                    ControlErrorCode::InternalError,
                    "failed to start CPU profiler",
                    serde_json::json!({ "error": err.to_string(), "frequency_hz": frequency_hz }),
                )
            })?;

        Ok(Box::new(PprofProfilerEngine {
            guard,
            svg_path: svg_path.to_path_buf(),
        }))
    }
}

#[cfg(not(unix))]
mod platform {
    use super::*;

    pub(super) fn profiling_compiled_in() -> bool {
        false
    }

    pub(super) fn runtime_cpu_profile_supported() -> bool {
        false
    }

    pub(super) fn profile_formats() -> &'static [ProfileArtifactFormat] {
        &[]
    }

    pub(super) fn start_profiler(
        _frequency_hz: i32,
        _svg_path: &Path,
    ) -> Result<Box<dyn ProfilerEngine>, ControlError> {
        Err(ControlError::new(
            ControlErrorCode::RuntimeProfilingUnsupported,
            "runtime CPU profiling is not supported in this build",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeProfilerEngine {
        stop_calls: Arc<Mutex<Vec<PathBuf>>>,
        svg_path: PathBuf,
        stop_error: Option<ControlError>,
    }

    impl ProfilerEngine for FakeProfilerEngine {
        fn stop(self: Box<Self>) -> Result<(), ControlError> {
            self.stop_calls.lock().unwrap().push(self.svg_path.clone());
            if let Some(err) = self.stop_error {
                return Err(err);
            }
            fs::write(&self.svg_path, "main;work 42\n").unwrap();
            Ok(())
        }
    }

    fn fake_error(code: ControlErrorCode, message: &str) -> ControlError {
        ControlError::new(code, message)
    }

    fn test_active_profile(svg_path: PathBuf, engine: Box<dyn ProfilerEngine>) -> ActiveCpuProfile {
        ActiveCpuProfile {
            started_at: now_timestamp(),
            frequency_hz: DEFAULT_FREQUENCY_HZ,
            svg_path,
            engine,
        }
    }

    #[test]
    fn default_status_is_inactive() {
        let manager = CpuProfileManager::new();
        assert_eq!(manager.status(), CpuProfileStatus::Inactive);
    }

    #[test]
    fn reports_platform_capabilities() {
        let manager = CpuProfileManager::new();
        #[cfg(unix)]
        {
            assert!(manager.profiling_compiled_in());
            assert!(manager.runtime_cpu_profile());
            // Empty until the fleet can decode `Folded`; see
            // `platform::profile_formats()` for the wire-migration plan.
            assert_eq!(manager.profile_formats(), &[] as &[ProfileArtifactFormat]);
        }
        #[cfg(not(unix))]
        {
            assert!(!manager.profiling_compiled_in());
            assert!(!manager.runtime_cpu_profile());
            assert!(manager.profile_formats().is_empty());
        }
    }

    #[test]
    fn invalid_frequency_is_rejected() {
        let err = validate_frequency(Some(0)).unwrap_err();
        assert_eq!(err.code, ControlErrorCode::InvalidFrequency);

        let err = validate_frequency(Some(4001)).unwrap_err();
        assert_eq!(err.code, ControlErrorCode::InvalidFrequency);
    }

    #[test]
    fn output_collision_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("existing.folded");
        fs::write(&path, "already here").unwrap();

        let err = resolve_svg_path(Some(&path), &now_timestamp()).unwrap_err();
        assert_eq!(err.code, ControlErrorCode::OutputPathCollision);
    }

    #[test]
    fn directory_output_derives_filename() {
        let tmp = TempDir::new().unwrap();
        let started_at = now_timestamp();
        let resolved = resolve_svg_path(Some(tmp.path()), &started_at).unwrap();
        assert_eq!(resolved.path.parent(), Some(tmp.path()));
        assert_eq!(
            resolved.path.extension().and_then(|ext| ext.to_str()),
            Some("folded")
        );
        assert!(
            resolved
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(&started_at)
        );
    }

    #[test]
    fn basename_output_derives_filename() {
        let tmp = TempDir::new().unwrap();
        let started_at = now_timestamp();
        let resolved = resolve_svg_path(Some(&tmp.path().join("profile")), &started_at).unwrap();
        let name = resolved.path.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("profile-"));
        assert!(name.contains(&started_at));
        assert!(name.ends_with(".folded"));
    }

    #[test]
    fn explicit_svg_output_redirects_to_folded() {
        let tmp = TempDir::new().unwrap();
        let resolved =
            resolve_svg_path(Some(&tmp.path().join("profile.svg")), &now_timestamp()).unwrap();
        assert_eq!(resolved.path, tmp.path().join("profile.folded"));
    }

    #[test]
    fn auto_generated_filenames_retry_on_collision() {
        let tmp = TempDir::new().unwrap();
        let started_at = now_timestamp();
        let first = tmp
            .path()
            .join(candidate_filename("leader", &started_at, 0));
        fs::write(&first, "collision").unwrap();

        let resolved = resolve_svg_path(Some(tmp.path()), &started_at).unwrap();
        assert_ne!(resolved.path, first);
        assert_eq!(
            resolved.path.file_name().unwrap().to_string_lossy(),
            candidate_filename("leader", &started_at, 1)
        );
    }

    #[test]
    fn missing_parent_directory_is_created() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nested").join("profile.folded");
        let resolved = resolve_svg_path(Some(&path), &now_timestamp()).unwrap();
        assert_eq!(resolved.path, path);
        assert!(resolved.path.parent().unwrap().is_dir());
    }

    #[test]
    fn stop_without_active_profile_fails() {
        let mut manager = CpuProfileManager::new();
        let err = manager.stop().unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ProfileNotActive);
    }

    #[test]
    fn finalize_on_shutdown_without_profile_returns_none() {
        let mut manager = CpuProfileManager::new();
        assert_eq!(manager.finalize_on_shutdown().unwrap(), None);
    }

    #[test]
    fn supported_manager_lifecycle_works() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let stop_calls = Arc::new(Mutex::new(Vec::new()));
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: stop_calls.clone(),
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        let status = manager
            .start_with_engine_for_test(
                CpuProfileStartOptions {
                    output: Some(svg_path.clone()),
                    frequency_hz: Some(1000),
                },
                engine,
            )
            .unwrap();

        match status {
            CpuProfileStatus::Active {
                svg_path: status_path,
                frequency_hz,
                ..
            } => {
                assert_eq!(status_path, svg_path);
                assert_eq!(frequency_hz, 1000);
            }
            CpuProfileStatus::Inactive | CpuProfileStatus::Stopping { .. } => {
                panic!("expected active status")
            }
        }

        assert!(matches!(manager.status(), CpuProfileStatus::Active { .. }));

        let stop_result = manager.stop().unwrap();
        assert_eq!(stop_result.svg_path, svg_path);
        assert!(stop_result.stopped_at >= stop_result.started_at);
        assert_eq!(
            stop_calls.lock().unwrap().as_slice(),
            std::slice::from_ref(&svg_path)
        );
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }

    #[test]
    fn duplicate_start_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let stop_calls = Arc::new(Mutex::new(Vec::new()));
        let engine = Box::new(FakeProfilerEngine {
            stop_calls,
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        manager
            .start_with_engine_for_test(
                CpuProfileStartOptions {
                    output: Some(svg_path.clone()),
                    frequency_hz: Some(1000),
                },
                engine,
            )
            .unwrap();

        let duplicate = manager.start_with_engine_for_test(
            CpuProfileStartOptions {
                output: Some(tmp.path().join("profile2.folded")),
                frequency_hz: Some(1000),
            },
            Box::new(FakeProfilerEngine::default()),
        );
        let err = duplicate.unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ProfileAlreadyActive);
    }

    #[test]
    fn manager_start_rejects_unsupported_build() {
        #[cfg(not(unix))]
        {
            let mut manager = CpuProfileManager::new();
            let err = manager
                .start(CpuProfileStartOptions::default())
                .unwrap_err();
            assert_eq!(err.code, ControlErrorCode::RuntimeProfilingUnsupported);
        }
    }

    #[test]
    fn finalize_on_shutdown_stops_active_profile() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let stop_calls = Arc::new(Mutex::new(Vec::new()));
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: stop_calls.clone(),
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(svg_path.clone(), engine));

        let result = manager.finalize_on_shutdown().unwrap().unwrap();
        assert_eq!(result.svg_path, svg_path);
        assert_eq!(stop_calls.lock().unwrap().len(), 1);
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }

    #[test]
    fn take_stop_handle_marks_profile_as_stopping_until_completed() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(svg_path.clone(), engine));

        let stop_handle = manager.take_stop_handle().unwrap();
        assert!(matches!(
            manager.status(),
            CpuProfileStatus::Stopping {
                svg_path: status_path,
                ..
            } if status_path == svg_path
        ));

        let err = manager
            .start_with_engine_for_test(
                CpuProfileStartOptions {
                    output: Some(tmp.path().join("profile2.folded")),
                    frequency_hz: Some(1000),
                },
                Box::new(FakeProfilerEngine::default()),
            )
            .unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ProfileStopInProgress);

        let err = manager.take_stop_handle().unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ProfileStopInProgress);

        let stop_result = stop_handle.finish().unwrap();
        manager.complete_stop();
        assert_eq!(stop_result.svg_path, svg_path);
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }

    #[test]
    fn stop_failure_keeps_stopping_state_until_completed() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path: svg_path.clone(),
            stop_error: Some(fake_error(
                ControlErrorCode::ArtifactWriteFailed,
                "failed to write artifact",
            )),
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(svg_path.clone(), engine));

        let stop_handle = manager.take_stop_handle().unwrap();
        let err = stop_handle.finish().unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ArtifactWriteFailed);
        assert!(matches!(
            manager.status(),
            CpuProfileStatus::Stopping { .. }
        ));

        manager.complete_stop();
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }

    #[test]
    fn stop_clears_stopping_state_on_success() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(svg_path.clone(), engine));

        let result = manager.stop().unwrap();
        assert_eq!(result.svg_path, svg_path);
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }

    #[test]
    fn stop_clears_stopping_state_on_error() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path,
            stop_error: Some(fake_error(
                ControlErrorCode::ArtifactWriteFailed,
                "failed to write artifact",
            )),
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(
            tmp.path().join("profile.folded"),
            engine,
        ));

        let err = manager.stop().unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ArtifactWriteFailed);
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }

    #[test]
    fn take_shutdown_stop_handle_returns_none_when_stop_already_in_progress() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(svg_path, engine));
        let _stop_handle = manager.take_stop_handle().unwrap();

        assert_eq!(
            manager.shutdown_stop_disposition(),
            Some(ShutdownStopDisposition::AlreadyStopping)
        );
        assert!(manager.take_shutdown_stop_handle().unwrap().is_none());
    }

    #[test]
    fn status_surfaces_stopping_state() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path: svg_path.clone(),
            stop_error: None,
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(svg_path.clone(), engine));
        let _stop_handle = manager.take_stop_handle().unwrap();

        assert!(matches!(
            manager.status(),
            CpuProfileStatus::Stopping {
                svg_path: status_path,
                frequency_hz: DEFAULT_FREQUENCY_HZ,
                ..
            } if status_path == svg_path
        ));
    }

    #[test]
    fn stop_preserves_engine_error() {
        let tmp = TempDir::new().unwrap();
        let svg_path = tmp.path().join("profile.folded");
        let engine = Box::new(FakeProfilerEngine {
            stop_calls: Arc::new(Mutex::new(Vec::new())),
            svg_path,
            stop_error: Some(fake_error(
                ControlErrorCode::ArtifactWriteFailed,
                "failed to write artifact",
            )),
        });

        let mut manager = CpuProfileManager::new();
        manager.active = Some(test_active_profile(
            tmp.path().join("profile.folded"),
            engine,
        ));

        let err = manager.stop().unwrap_err();
        assert_eq!(err.code, ControlErrorCode::ArtifactWriteFailed);
        assert!(matches!(manager.status(), CpuProfileStatus::Inactive));
    }
}
