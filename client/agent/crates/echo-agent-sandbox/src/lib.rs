#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! OS-level sandboxing for EchoAgent Build via [nono](https://crates.io/crates/nono).
//!
//! Applied once at process startup. Covers in-process `tokio::fs` calls
//! and child processes. Network is left open at the process level (agent
//! needs LLM API); child network is blocked per-subprocess via seccomp.
//!
//! The `enforce` feature (on by default) pulls in `nono` for
//! kernel-enforced sandboxing (Landlock/Seatbelt). When disabled, the
//! crate still provides lightweight helpers (`log_violation`,
//! `should_restrict_child_network`, `child_net`) that compile on all
//! targets including musl.
//!
//! ```rust,no_run
//! use echo_agent_sandbox::{SandboxManager, ProfileName};
//! use std::path::Path;
//!
//! let workspace = Path::new("/home/user/project");
//! let mut sandbox = SandboxManager::new(ProfileName::Workspace, workspace);
//! sandbox.apply(workspace).expect("sandbox apply failed");
//! sandbox.install();
//! ```
mod allow_path;
pub mod child_net;
mod deny;
mod hook_write_deny;
mod logging;
mod network_policy;
mod paths;
mod profiles;
mod types;
pub use hook_write_deny::{profile_enforces_hook_write_deny, verify_hook_write_deny_enforced};
pub use logging::SandboxLogger;
pub use network_policy::{
    ChildNetworkPolicy, NETWORK_POLICY_SNAPSHOT_VERSION, NetworkPolicySnapshot,
    NetworkPolicySnapshotError, WebsiteAction, WebsiteOrigin, WebsiteOriginError, WebsitePolicy,
};
pub use profiles::{
    ProfileName, SandboxConfig, SandboxProfile, load_sandbox_config, sandbox_profile_conflicts,
};
pub use types::{SandboxEvent, SandboxEventType, SandboxMetrics};
/// Whether this profile requires direct-hook write protection (non-devbox
/// enforcing profiles). Shell fails closed when protection cannot be applied.
pub fn requires_hook_write_deny(profile: &ProfileName, workspace: &Path) -> bool {
    if !profile_enforces_hook_write_deny(profile) || *profile == ProfileName::Off {
        return false;
    }
    let config = profiles::load_sandbox_config(workspace);
    match profile {
        ProfileName::Custom(name) => {
            config.profiles.get(name).and_then(|p| p.extends.as_deref()) != Some("devbox")
        }
        ProfileName::Devbox => false,
        _ => true,
    }
}
#[cfg(all(feature = "enforce", unix))]
use nono::Sandbox;
use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
static SANDBOX: OnceLock<GlobalSandboxState> = OnceLock::new();
static CONFIGURED_PROFILE: OnceLock<String> = OnceLock::new();
static AUTO_ALLOW_BASH: AtomicBool = AtomicBool::new(false);
const BWRAP_ENV_VAR: &str = "__ECHO_AGENT_INSIDE_BWRAP";
pub fn is_inside_bwrap() -> bool {
    std::env::var(BWRAP_ENV_VAR).is_ok()
}
pub fn trust_bwrap_marker_for_devbox() -> bool {
    false
}
struct GlobalSandboxState {
    profile: String,
    logger: SandboxLogger,
    applied: bool,
    restrict_network_at_known_linux_launches: bool,
}
fn restrict_network_at_known_linux_launches(applied: bool, configured: bool) -> bool {
    applied && configured && cfg!(target_os = "linux")
}
/// Whether known Linux child launch paths should install the seccomp network filter.
pub fn should_restrict_child_network() -> bool {
    SANDBOX
        .get()
        .is_some_and(|state| state.restrict_network_at_known_linux_launches)
}
/// Whether bash commands should be auto-approved when the sandbox is active.
pub fn should_auto_allow_bash() -> bool {
    AUTO_ALLOW_BASH.load(Ordering::Relaxed) && is_active()
}
pub fn set_auto_allow_bash(enabled: bool) {
    AUTO_ALLOW_BASH.store(enabled, Ordering::Relaxed);
}
/// Record the resolved sandbox profile at process startup (including `"off"`).
pub fn set_configured_profile(name: impl Into<String>) {
    let _ = CONFIGURED_PROFILE.set(name.into());
}
/// Resolved sandbox profile from startup, or `None` if `set_configured_profile` was never called.
pub fn configured_profile_name() -> Option<&'static str> {
    CONFIGURED_PROFILE.get().map(|s| s.as_str())
}
/// The non-`off` sandbox profile this process was **requested** with, if any.
///
/// This is the configured request, not a report that enforcement succeeded —
/// `is_active()` can be false while the process is still confined (e.g. some
/// Linux bwrap paths), and a requested-but-unapplied profile already warns the
/// user. Keying on the request is the fail-closed choice.
pub fn requested_confinement_profile() -> Option<&'static str> {
    configured_profile_name().filter(|name| profile_confines(name))
}
fn profile_confines(name: &str) -> bool {
    name.parse::<ProfileName>()
        .is_ok_and(|profile| profile != ProfileName::Off)
}
/// Whether the sandbox was successfully applied to this process.
pub fn is_active() -> bool {
    SANDBOX.get().is_some_and(|s| s.applied)
}
/// The active sandbox profile name, or `None` if sandbox is not applied.
pub fn profile_name() -> Option<&'static str> {
    SANDBOX
        .get()
        .filter(|s| s.applied)
        .map(|s| s.profile.as_str())
}
/// Log a sandbox violation. Immediately flushed to disk.
/// No-op if sandbox is not active.
pub fn log_violation(target: &str, operation: &str) {
    if let Some(state) = SANDBOX.get() {
        state.logger.log(SandboxEvent::fs_violation(
            &state.profile,
            target,
            operation,
        ));
        let _ = state.logger.flush_to_disk();
    }
}
/// Flush sandbox events to disk. No-op if not initialized.
pub fn flush() {
    if let Some(state) = SANDBOX.get()
        && let Err(e) = state.logger.flush_to_disk()
    {
        tracing::warn!(error = %e, "Failed to flush sandbox events to disk");
    }
}
/// Violation metrics, or `None` if sandbox is not active.
pub fn metrics() -> Option<&'static SandboxMetrics> {
    SANDBOX.get().map(|s| s.logger.metrics())
}
/// Manages the OS-level sandbox. Call `apply()` then `install()`.
pub struct SandboxManager {
    profile: ProfileName,
    logger: SandboxLogger,
    net_restricted: bool,
    applied: bool,
}
impl SandboxManager {
    /// Create a sandbox manager. Does not apply until `apply()` is called.
    pub fn new(profile: ProfileName, _workspace: &Path) -> Self {
        let net_restricted = profile.restricts_network();
        Self {
            profile,
            logger: SandboxLogger::new(),
            net_restricted,
            applied: false,
        }
    }
    /// Apply the sandbox to the current process. **Irreversible.**
    /// Degrades gracefully if the platform doesn't support it.
    #[cfg(all(feature = "enforce", unix))]
    pub fn apply(&mut self, workspace: &Path) -> anyhow::Result<()> {
        if self.profile == ProfileName::Off {
            tracing::info!("Sandbox disabled (profile: off)");
            return Ok(());
        }
        if requires_hook_write_deny(&self.profile, workspace) {
            echo_agent_config::ensure_echo_agent_hook_slots(paths::echo_agent_home().as_path())
                .map_err(|e| anyhow::anyhow!("hook write-deny ensure failed: {e}"))?;
            hook_write_deny::maybe_install_namespace_lockdown_inside_bwrap(&self.profile)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        let config = profiles::load_sandbox_config(workspace);
        let mut resolved = self.profile.resolve_profile(workspace, &config)?;
        self.net_restricted = resolved.restrict_network;
        let support = Sandbox::support_info();
        if !support.is_supported {
            tracing::warn!(
                details = %support.details,
                "Sandbox not supported on this platform, continuing without sandbox"
            );
            self.logger.log(SandboxEvent::apply_failed(
                &self.profile.to_string(),
                workspace,
                &support.details,
            ));
            return Ok(());
        }
        let caps = ProfileName::capability_set_from_profile(workspace, &resolved)?;
        resolved.deny = deny::effective_deny_paths(workspace, &resolved.deny);
        match Sandbox::apply(&caps) {
            Ok(_) => {
                self.applied = true;
                self.logger.log(SandboxEvent::profile_applied(
                    &self.profile.to_string(),
                    workspace,
                    &resolved,
                ));
                tracing::info!(
                    profile = %self.profile,
                    workspace = %workspace.display(),
                    restrict_network_configured = self.net_restricted,
                    "Sandbox applied (kernel-enforced, irreversible)"
                );
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    profile = %self.profile,
                    error = %e,
                    "Sandbox could not be applied, continuing without sandbox"
                );
                self.logger.log(SandboxEvent::apply_failed(
                    &self.profile.to_string(),
                    workspace,
                    &e,
                ));
                Ok(())
            }
        }
    }
    /// Stub when `enforce` feature is disabled — sandbox is not applied.
    #[cfg(not(all(feature = "enforce", unix)))]
    pub fn apply(&mut self, _workspace: &Path) -> anyhow::Result<()> {
        tracing::info!(
            profile = %self.profile,
            "Sandbox enforcement unavailable (built without 'enforce' feature)"
        );
        Ok(())
    }
    /// Store globally for session-lifetime violation logging.
    pub fn install(self) {
        let _ = self.logger.flush_to_disk();
        let _ = SANDBOX.set(GlobalSandboxState {
            profile: self.profile.to_string(),
            logger: self.logger,
            applied: self.applied,
            restrict_network_at_known_linux_launches: restrict_network_at_known_linux_launches(
                self.applied,
                self.net_restricted,
            ),
        });
    }
    /// Check whether the current platform supports sandboxing.
    #[cfg(all(feature = "enforce", unix))]
    pub fn support_info() -> nono::SupportInfo {
        Sandbox::support_info()
    }
    /// Whether the sandbox was successfully applied.
    pub fn is_applied(&self) -> bool {
        self.applied
    }
    /// Whether known Linux child launch paths should install the seccomp network filter.
    pub fn restrict_child_network(&self) -> bool {
        restrict_network_at_known_linux_launches(self.applied, self.net_restricted)
    }
    /// The active profile name.
    pub fn profile(&self) -> &ProfileName {
        &self.profile
    }
    /// Access the sandbox event logger (before `install()`).
    pub fn logger(&self) -> &SandboxLogger {
        &self.logger
    }
}
/// Build a bwrap command that re-execs the current process with
/// `deny_write` paths mounted read-only and `deny_read` paths bound
/// over with an unreadable placeholder (EPERM on read).
///
/// Returns `None` if already inside bwrap. Caller should `cmd.exec()` the result.
pub fn bwrap_reexec_command(
    deny_write: &[&str],
    deny_read: &[&str],
) -> Option<std::process::Command> {
    #[cfg(target_os = "linux")]
    {
        bwrap_reexec_command_ex(deny_write, None, deny_read)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (deny_write, deny_read);
        None
    }
}
/// Like [`bwrap_reexec_command`] plus optional hook plan (via `append_hook_plan_binds`).
#[cfg(target_os = "linux")]
pub(crate) fn bwrap_reexec_command_ex(
    deny_write_optional: &[&str],
    hook_plan: Option<&hook_write_deny::HookWriteDenyBwrapPlan>,
    deny_read: &[&str],
) -> Option<std::process::Command> {
    if is_inside_bwrap() {
        return None;
    }
    let self_exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("error: could not resolve the current executable for the bwrap re-exec: {e}");
            return None;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cmd = std::process::Command::new("bwrap");
    cmd.arg("--cap-drop").arg("ALL");
    cmd.arg("--bind").arg("/").arg("/");
    for path in deny_write_optional {
        if Path::new(path).exists() {
            cmd.arg("--ro-bind").arg(path).arg(path);
        }
    }
    if let Some(plan) = hook_plan
        && let Err(e) = hook_write_deny::append_hook_plan_binds(&mut cmd, plan)
    {
        eprintln!("error: hook write-deny plan materialization failed: {e}");
        return None;
    }
    if !deny_read.is_empty() {
        for path in deny_read {
            let Some(blocked) = bwrap_blocked_source_for_path(Path::new(path)) else {
                eprintln!(
                    "error: could not create bwrap placeholder for read-deny path {path}; \
                     refusing to start with a partial sandbox"
                );
                return None;
            };
            cmd.arg("--ro-bind").arg(&blocked).arg(path);
        }
    }
    cmd.arg("--dev-bind").arg("/dev").arg("/dev");
    cmd.arg("--proc").arg("/proc");
    cmd.env(BWRAP_ENV_VAR, "1");
    cmd.arg("--").arg(self_exe).args(args);
    Some(cmd)
}
/// Choose file vs directory placeholder for a deny path (existing dirs need a dir bind).
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn bwrap_blocked_source_for_path(path: &Path) -> Option<PathBuf> {
    if deny::deny_path_is_dir(path) {
        bwrap_blocked_placeholder("sandbox-blocked-dir", true)
    } else {
        bwrap_blocked_placeholder("sandbox-blocked", false)
    }
}
/// Without kernel enforcement there are no read-deny placeholders to bind over.
#[cfg(all(not(feature = "enforce"), target_os = "linux"))]
fn bwrap_blocked_source_for_path(_path: &Path) -> Option<PathBuf> {
    None
}
/// chmod a placeholder to mode 000 so a bwrap bind-over yields EPERM on read.
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn chmod_000(path: &Path) -> Option<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).ok()?.permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(path, perms).ok()?;
    Some(())
}
/// Zero-permission placeholder (file or dir) under `echo_agent_home` used by bwrap bind-over.
///
/// The placeholder name is suffixed with the current PID so concurrent echoagent
/// processes don't race each other's create/remove/chmod on a shared path (which
/// could yield `None` and the silent dropped-bind fail-open this avoids).
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn bwrap_blocked_placeholder(name: &str, want_dir: bool) -> Option<PathBuf> {
    use std::fs::OpenOptions;
    let path = paths::echo_agent_home().join(format!("{name}.{}", std::process::id()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    if path.exists() {
        if path.is_dir() == want_dir {
            chmod_000(&path)?;
            return Some(path);
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path).ok()?;
        } else {
            std::fs::remove_file(&path).ok()?;
        }
    }
    if want_dir {
        std::fs::create_dir(&path).ok()?;
    } else {
        OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .ok()?;
    }
    chmod_000(&path)?;
    Some(path)
}
/// Whether a profile write-denies `/data` via the devbox bwrap bind (built-in
/// `devbox` or a custom profile that `extends = "devbox"`). This is a pure mount,
/// so it applies even WITHOUT the `enforce` feature.
#[cfg(target_os = "linux")]
fn is_devbox_based(profile: &ProfileName, config: &SandboxConfig) -> bool {
    match profile {
        ProfileName::Devbox => true,
        ProfileName::Custom(name) => {
            config.profiles.get(name).and_then(|p| p.extends.as_deref()) == Some("devbox")
        }
        _ => false,
    }
}
/// Whether kernel read-deny enforcement is required. The single source of truth
/// for this classification so callers (e.g. the shell's fail-closed startup path)
/// cannot drift and silently fail open.
///
/// Decided directly from the profile config (a `Custom` profile with a non-empty
/// `deny`) — NOT from the resolved/expanded deny set, which returns empty on
/// failure. Keying "requires" on that empty-on-error result would silently
/// downgrade to fail-open (Linux) when resolution hiccups; this intrinsic check
/// stays fail-closed.
#[cfg(all(feature = "enforce", unix))]
pub fn requires_read_deny(profile: &ProfileName, workspace: &Path) -> bool {
    match profile {
        ProfileName::Custom(name) => {
            let config = profiles::load_sandbox_config(workspace);
            config
                .profiles
                .get(name)
                .is_some_and(|p| !p.deny.is_empty())
        }
        _ => false,
    }
}
/// Stub when `enforce` is unavailable — nothing is kernel-enforced.
#[cfg(not(all(feature = "enforce", unix)))]
pub fn requires_read_deny(_profile: &ProfileName, _workspace: &Path) -> bool {
    false
}
/// Whether a `resolve_profile` failure must refuse startup: any profile that
/// enforces hook write-deny or its own deny list cannot proceed with an empty
/// plan. The read-deny arm covers deny-carrying `extends = "devbox"` profiles,
/// which the hook arm does not; devbox resolution is infallible today, so that
/// arm is defense in depth against a future fallible resolve step.
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn resolve_failure_must_refuse(profile: &ProfileName, workspace: &Path) -> bool {
    requires_hook_write_deny(profile, workspace) || requires_read_deny(profile, workspace)
}
/// A profile's resolved bwrap deny plan.
#[cfg(target_os = "linux")]
struct BwrapDenyPlan {
    deny_write_optional: Vec<String>,
    hook_plan: Option<hook_write_deny::HookWriteDenyBwrapPlan>,
    deny_read: Vec<String>,
    has_globs: bool,
}
#[cfg(all(feature = "enforce", target_os = "linux"))]
fn bwrap_deny_plan(profile: &ProfileName, workspace: &Path) -> Option<BwrapDenyPlan> {
    let config = profiles::load_sandbox_config(workspace);
    let deny_write_optional: Vec<String> = if is_devbox_based(profile, &config) {
        vec!["/data".to_string()]
    } else {
        Vec::new()
    };
    let resolved = if *profile == ProfileName::Off {
        None
    } else {
        match profile.resolve_profile(workspace, &config) {
            Ok(r) => Some(r),
            Err(e) => {
                if resolve_failure_must_refuse(profile, workspace) {
                    eprintln!("error: sandbox profile resolve failed: {e}");
                    return None;
                }
                None
            }
        }
    };
    let entries = resolved
        .as_ref()
        .map(|r| r.deny.clone())
        .unwrap_or_default();
    let needs_hooks = requires_hook_write_deny(profile, workspace);
    let hook_plan = if needs_hooks {
        match hook_write_deny::prepare_hook_write_deny(profile) {
            Ok(hook_write_deny::HookWriteDenyPrepare::NotRequired) => None,
            Ok(hook_write_deny::HookWriteDenyPrepare::Plan(plan)) => Some(plan),
            Err(e) => {
                eprintln!("error: hook write-deny plan failed: {e}");
                return None;
            }
        }
    } else {
        None
    };
    if needs_hooks && hook_plan.is_none() {
        eprintln!("error: hook write-deny is required but no plan was prepared");
        return None;
    }
    let (exact, globs) = deny::partition_deny_entries(&entries);
    let mut deny_read = deny::exact_deny_path_strings(workspace, &exact);
    let has_globs = !globs.is_empty();
    if has_globs {
        tracing::warn!(
            count = globs.len(),
            "sandbox deny globs are enforced best-effort on Linux (expanded at launch); \
             files matching them that are created later are NOT covered"
        );
        match deny::expand_deny_globs(workspace, &globs, deny::DENY_GLOB_CAPS) {
            Ok(paths) => deny_read.extend(paths),
            Err(reason) => {
                tracing::error!(%reason, "sandbox deny-glob expansion failed; refusing to start");
                eprintln!("error: sandbox deny glob could not be enforced on Linux: {reason}");
                return None;
            }
        }
    }
    Some(BwrapDenyPlan {
        deny_write_optional,
        hook_plan,
        deny_read,
        has_globs,
    })
}
/// Without kernel enforcement there is no read-deny; the devbox `/data`
/// write-deny and the hook write-deny plan still apply.
#[cfg(all(not(feature = "enforce"), target_os = "linux"))]
fn bwrap_deny_plan(profile: &ProfileName, workspace: &Path) -> Option<BwrapDenyPlan> {
    let config = profiles::load_sandbox_config(workspace);
    let deny_write_optional: Vec<String> = if is_devbox_based(profile, &config) {
        vec!["/data".to_string()]
    } else {
        Vec::new()
    };
    let hook_plan = if requires_hook_write_deny(profile, workspace) {
        match hook_write_deny::prepare_hook_write_deny(profile) {
            Ok(hook_write_deny::HookWriteDenyPrepare::NotRequired) => None,
            Ok(hook_write_deny::HookWriteDenyPrepare::Plan(plan)) => Some(plan),
            Err(e) => {
                eprintln!("error: hook write-deny plan failed: {e}");
                return None;
            }
        }
    } else {
        None
    };
    Some(BwrapDenyPlan {
        deny_write_optional,
        hook_plan,
        deny_read: Vec::new(),
        has_globs: false,
    })
}
/// Build the bwrap re-exec command a profile needs on Linux. Returns `None`
/// when no re-exec is needed (already inside bwrap, nothing to deny) or the
/// deny plan could not be built; plan failures print their cause.
#[cfg(target_os = "linux")]
pub fn bwrap_reexec_for_profile(
    profile: &ProfileName,
    workspace: &Path,
) -> Option<std::process::Command> {
    if is_inside_bwrap() {
        return None;
    }
    let BwrapDenyPlan {
        deny_write_optional,
        hook_plan,
        deny_read,
        has_globs,
    } = bwrap_deny_plan(profile, workspace)?;
    if deny_write_optional.is_empty() && hook_plan.is_none() && deny_read.is_empty() && !has_globs {
        return None;
    }
    let write_opt: Vec<&str> = deny_write_optional.iter().map(String::as_str).collect();
    let read_refs: Vec<&str> = deny_read.iter().map(String::as_str).collect();
    bwrap_reexec_command_ex(&write_opt, hook_plan.as_ref(), &read_refs)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    /// Save, set/remove, and auto-restore an env var on drop.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe { std::env::set_var(key, val) };
            Self { key, prev }
        }
        fn remove(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }
    #[test]
    #[serial(bwrap_env)]
    fn bwrap_reexec_returns_none_inside_bwrap() {
        let _g = EnvGuard::set(BWRAP_ENV_VAR, "1");
        let result = bwrap_reexec_command(&["/data"], &[]);
        assert!(
            result.is_none(),
            "should return None when already inside bwrap"
        );
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(target_os = "linux")]
    fn bwrap_reexec_returns_some_outside_bwrap() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let result = bwrap_reexec_command(&["/tmp"], &[]);
        assert!(result.is_some(), "should return Some when not inside bwrap");
        let cmd = result.unwrap();
        assert_eq!(cmd.get_program(), "bwrap", "program should be bwrap");
    }
    #[test]
    #[serial(bwrap_env)]
    fn trust_bwrap_marker_for_devbox_tracks_env_when_feature_on() {
        let _g = EnvGuard::set(BWRAP_ENV_VAR, "1");
        assert!(
            !trust_bwrap_marker_for_devbox(),
            "without bwrap-marker the hatch must stay closed"
        );
    }
    #[test]
    #[serial(bwrap_env)]
    fn trust_bwrap_marker_for_devbox_false_outside_bwrap() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        assert!(!trust_bwrap_marker_for_devbox());
        assert!(!is_inside_bwrap());
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(target_os = "linux")]
    fn bwrap_reexec_skips_nonexistent_paths() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let result = bwrap_reexec_command(&["/nonexistent-test-path-xyz-12345"], &[]);
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(
            !args.iter().any(|a| a == "/nonexistent-test-path-xyz-12345"),
            "should skip non-existent deny_write paths, got args: {args:?}"
        );
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    fn bwrap_reexec_binds_nonexistent_deny_read_paths() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let missing = "/nonexistent-deny-read-path-xyz-12345";
        let result = bwrap_reexec_command(&[], &[missing]);
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let has_bind = args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[2] == missing);
        assert!(
            has_bind,
            "should bind-over non-existent deny_read paths, got args: {args:?}"
        );
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(target_os = "linux")]
    fn bwrap_reexec_mounts_existing_paths_read_only() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let result = bwrap_reexec_command(&["/tmp"], &[]);
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let has_ro_bind = args.windows(3).any(|w| w == ["--ro-bind", "/tmp", "/tmp"]);
        assert!(
            has_ro_bind,
            "should mount existing paths as --ro-bind, got args: {args:?}"
        );
    }
    /// Hook plan: rootward ancestor RW self-binds precede leaf RO; no bwrap
    /// version flags required. Identity revalidation is part of append.
    #[test]
    #[serial(bwrap_env)]
    #[cfg(target_os = "linux")]
    fn bwrap_hook_plan_binds_ancestors_then_leaves() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let root = std::env::temp_dir().join(format!(
            "echo-agent-bwrap-hook-plan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let parent = root.join("sessions");
        let leaf = parent.join("extra-hooks");
        std::fs::create_dir_all(&leaf).unwrap();
        let sources = [echo_agent_config::GlobalHookSource {
            path: leaf.clone(),
            kind: echo_agent_config::GlobalHookSourceKind::ConfiguredSource,
        }];
        let plan = hook_write_deny::build_bwrap_plan(&sources).expect("plan");
        assert!(
            !plan.ancestor_rw_binds.iter().any(|p| p == Path::new("/")),
            "must not RW-bind /: {:?}",
            plan.ancestor_rw_binds
        );
        assert!(
            plan.ancestor_rw_binds.iter().any(|p| p == &parent),
            "immediate parent must be pinned: {:?}",
            plan.ancestor_rw_binds
        );
        for w in plan.ancestor_rw_binds.windows(2) {
            assert!(
                w[0].components().count() <= w[1].components().count(),
                "ancestors not rootward: {:?}",
                plan.ancestor_rw_binds
            );
        }
        let moved = root.join("extra-hooks-old");
        std::fs::rename(&leaf, &moved).unwrap();
        std::fs::create_dir_all(&leaf).unwrap();
        let mut refuse = std::process::Command::new("bwrap");
        let err = hook_write_deny::append_hook_plan_binds(&mut refuse, &plan);
        assert!(err.is_err(), "must refuse replaced leaf identity");
        let _ = std::fs::remove_dir_all(&leaf);
        std::fs::rename(&moved, &leaf).unwrap();
        let plan = hook_write_deny::build_bwrap_plan(&sources).expect("plan2");
        let cmd = bwrap_reexec_command_ex(&[], Some(&plan), &[]).expect("bwrap command");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert!(
            !args.iter().any(|a| a == "--disable-userns"),
            "must not require --disable-userns: {args:?}"
        );
        assert!(
            args.windows(2).any(|w| w == ["--cap-drop", "ALL"]),
            "expected --cap-drop ALL: {args:?}"
        );
        let parent_s = parent.to_string_lossy().to_string();
        let leaf_s = leaf.to_string_lossy().to_string();
        let anc_parent = args
            .windows(3)
            .position(|w| w[0] == "--bind" && w[1] == parent_s && w[2] == parent_s);
        let leaf_pos = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == leaf_s && w[2] == leaf_s);
        assert!(anc_parent.is_some(), "expected RW bind of parent: {args:?}");
        assert!(leaf_pos.is_some(), "expected RO bind of leaf: {args:?}");
        assert!(
            anc_parent.unwrap() < leaf_pos.unwrap(),
            "ancestor RW must precede leaf RO; args: {args:?}"
        );
        for anc in &plan.ancestor_rw_binds {
            let a = anc.to_string_lossy().to_string();
            let pos = args
                .windows(3)
                .position(|w| w[0] == "--bind" && w[1] == a && w[2] == a);
            assert!(pos.is_some(), "missing RW bind for {a}: {args:?}");
            assert!(pos.unwrap() < leaf_pos.unwrap());
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(target_os = "linux")]
    fn bwrap_reexec_uses_dev_bind() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let result = bwrap_reexec_command(&[], &[]);
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let has_dev_bind = args.windows(3).any(|w| w == ["--dev-bind", "/dev", "/dev"]);
        assert!(
            has_dev_bind,
            "should use --dev-bind for /dev passthrough, got args: {args:?}"
        );
    }
    #[test]
    fn configured_profile_is_recorded() {
        set_configured_profile("read-only");
        assert_eq!(configured_profile_name(), Some("read-only"));
    }
    #[test]
    fn profile_confines_only_for_non_off_profiles() {
        assert!(!super::profile_confines("off"));
        assert!(!super::profile_confines("none"));
        assert!(super::profile_confines("strict"));
        assert!(super::profile_confines("read-only"));
        assert!(super::profile_confines("readonly"));
        assert!(super::profile_confines("my-custom-profile"));
    }
    #[test]
    fn known_launch_guard_is_linux_only() {
        assert_eq!(
            restrict_network_at_known_linux_launches(true, true),
            cfg!(target_os = "linux")
        );
        assert!(!restrict_network_at_known_linux_launches(false, true));
        assert!(!restrict_network_at_known_linux_launches(true, false));
    }
    /// Create a temp workspace whose `.echo-agent/sandbox.toml` contains `toml_body`.
    /// Returns the workspace path (caller removes it).
    #[cfg(all(feature = "enforce", unix))]
    fn temp_workspace_with_sandbox_toml(tag: &str, toml_body: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws =
            std::env::temp_dir().join(format!("echo-agent-{tag}-{}-{nanos}", std::process::id()));
        let echoagent = ws.join(".echo-agent");
        std::fs::create_dir_all(&echoagent).unwrap();
        std::fs::write(echoagent.join("sandbox.toml"), toml_body).unwrap();
        ws
    }
    /// Create a temp workspace defining a `denytest` profile (extends `workspace`)
    /// with the given `deny` list. `deny_toml` is the raw TOML array body
    /// (e.g. `"\".env\""`).
    #[cfg(all(feature = "enforce", unix))]
    fn temp_workspace_with_deny(tag: &str, deny_toml: &str) -> PathBuf {
        temp_workspace_with_sandbox_toml(
            tag,
            &format!("[profiles.denytest]\nextends = \"workspace\"\ndeny = [{deny_toml}]\n"),
        )
    }
    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn requires_read_deny_only_for_custom_profile_with_deny() {
        let ws = temp_workspace_with_deny("requires-deny", "\".env\"");
        assert!(requires_read_deny(
            &ProfileName::Custom("denytest".to_string()),
            &ws
        ));
        assert!(!requires_read_deny(
            &ProfileName::Custom("undefined".to_string()),
            &ws
        ));
        assert!(!requires_read_deny(&ProfileName::Workspace, &ws));
        assert!(!requires_read_deny(&ProfileName::Strict, &ws));
        assert!(!requires_read_deny(&ProfileName::Devbox, &ws));
        assert!(!requires_read_deny(&ProfileName::Off, &ws));
        let _ = std::fs::remove_dir_all(&ws);
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    fn bwrap_reexec_uses_dir_placeholder_for_directories() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let dir = std::env::temp_dir().join(format!("echo-agent-deny-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir_str = dir.to_string_lossy().to_string();
        let result = bwrap_reexec_command(&[], &[&dir_str]);
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let blocked_dir = paths::echo_agent_home()
            .join(format!("sandbox-blocked-dir.{}", std::process::id()))
            .to_string_lossy()
            .to_string();
        let has_dir_bind = args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == blocked_dir && w[2] == dir_str);
        assert!(
            has_dir_bind,
            "existing directories should bind over sandbox-blocked-dir, got args: {args:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    fn bwrap_reexec_for_profile_devbox_extends_composes_data_and_read_deny() {
        let _g = EnvGuard::remove(BWRAP_ENV_VAR);
        let ws = temp_workspace_with_sandbox_toml(
            "devbox-compose",
            "[profiles.devcustom]\nextends = \"devbox\"\ndeny = [\"secret.pem\"]\n",
        );
        let cmd = bwrap_reexec_for_profile(&ProfileName::Custom("devcustom".to_string()), &ws)
            .expect("devbox-extending custom with deny should build a re-exec command");
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let deny_path = ws.join("secret.pem").to_string_lossy().to_string();
        assert!(
            args.windows(3)
                .any(|w| w[0] == "--ro-bind" && w[2] == deny_path),
            "expected read-deny bind for {deny_path}, got args: {args:?}"
        );
        if Path::new("/data").exists() {
            assert!(
                args.windows(3)
                    .any(|w| w == ["--ro-bind", "/data", "/data"]),
                "expected /data write-deny ro-bind, got args: {args:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&ws);
        let ws_empty = temp_workspace_with_sandbox_toml(
            "devbox-empty",
            "[profiles.devempty]\nextends = \"devbox\"\n",
        );
        assert!(
            bwrap_reexec_for_profile(&ProfileName::Custom("devempty".to_string()), &ws_empty)
                .is_some(),
            "devbox-extending custom must compose the /data write-deny re-exec"
        );
        let _ = std::fs::remove_dir_all(&ws_empty);
        let ws_ws = temp_workspace_with_sandbox_toml(
            "ws-empty",
            "[profiles.wsempty]\nextends = \"workspace\"\n",
        );
        assert!(
            bwrap_reexec_for_profile(&ProfileName::Custom("wsempty".to_string()), &ws_ws).is_some(),
            "non-devbox custom must re-exec for direct-hook write-deny"
        );
        let _ = std::fs::remove_dir_all(&ws_ws);
    }
    #[test]
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    fn resolve_failure_refuses_for_deny_carrying_devbox_extends() {
        let workspace = temp_workspace_with_sandbox_toml(
            "resolve-refuse",
            "[profiles.devdeny]\nextends = \"devbox\"\ndeny = [\"secret.pem\"]\n",
        );
        let profile = ProfileName::Custom("devdeny".to_string());
        assert!(!requires_hook_write_deny(&profile, &workspace));
        assert!(resolve_failure_must_refuse(&profile, &workspace));
        let _ = std::fs::remove_dir_all(&workspace);
    }
    #[test]
    #[serial(bwrap_env)]
    #[cfg(all(feature = "enforce", target_os = "linux"))]
    fn bwrap_reexec_for_profile_skips_plan_inside_bwrap() {
        let _g = EnvGuard::set(BWRAP_ENV_VAR, "1");
        let workspace = temp_workspace_with_sandbox_toml(
            "inside-bwrap-skip",
            "[profiles.devskip]\nextends = \"devbox\"\ndeny = [\"secret.pem\"]\n",
        );
        assert!(
            bwrap_reexec_for_profile(&ProfileName::Custom("devskip".to_string()), &workspace)
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
