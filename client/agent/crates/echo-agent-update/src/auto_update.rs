use anyhow::{Context, Result};
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncWriteExt;

use crate::version::{
    UpdateConfig, fetch_latest_version, get_installed_echo_agent_version, get_latest_version,
    is_version_cache_fresh, try_fetch_stable_pointer, write_version_cache,
};
use echo_agent_runtime::util::config;
use echo_agent_runtime::util::echo_agent_home::{echo_agent_application, echo_agent_home};
pub use echo_agent_telemetry::events::CliUpdateTrigger;
use echo_agent_telemetry::events::{
    CliUpdate, CliUpdateChannel, CliUpdateErrorKind, CliUpdateInstaller, CliUpdateOutcome,
};

#[derive(Clone, Copy, Debug)]
pub enum UpdateRunMode {
    Blocking,
    NonBlocking,
}

const PROMPT_UPDATE_NOW: &str = "Update now? [Y/n/d]";
const MSG_AUTO_UPDATE_BACKGROUND: &str = "Auto-update running in background.";
const MSG_RUN_UPDATE_MANUAL: &str = "Run `echoagent update` to get the latest version.";
/// An empty or `"stable"` channel means stable — the installers' default
/// (`CHANNEL="${ECHO_AGENT_CHANNEL:-stable}"` in install.sh).
fn is_stable_channel(channel: &str) -> bool {
    channel.is_empty() || channel == "stable"
}

/// Manual-install one-liner for this platform's bootstrap installer.
///
/// On Unix the variable must prefix `bash` (which runs install.sh), not
/// `curl`: in `VAR=x curl … | bash` the assignment applies to `curl` only
/// and install.sh would fall back to stable.
fn manual_install_cmd(channel: &str) -> String {
    // Only interpolate a well-formed channel ([A-Za-z0-9._-]) into the
    // shell one-liner; anything else falls back to stable (a working
    // installer beats a broken quoted command).
    let channel = channel.trim();
    let safe = !channel.is_empty()
        && channel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if channel == "enterprise" {
        // Enterprise has its own bootstrap script; it needs no channel env.
        return if cfg!(windows) {
            "irm https://echo-agent.invalid/cli/enterprise-install.ps1 | iex".to_string()
        } else {
            "curl -fsSL https://echo-agent.invalid/cli/enterprise-install.sh | bash".to_string()
        };
    }
    if is_stable_channel(channel) || !safe {
        return if cfg!(windows) {
            "irm https://echo-agent.invalid/cli/install.ps1 | iex".to_string()
        } else {
            "curl -fsSL https://echo-agent.invalid/cli/install.sh | bash".to_string()
        };
    }
    if cfg!(windows) {
        format!(
            "$env:ECHO_AGENT_CHANNEL='{channel}'; irm https://echo-agent.invalid/cli/install.ps1 | iex"
        )
    } else {
        format!(
            "curl -fsSL https://echo-agent.invalid/cli/install.sh | ECHO_AGENT_CHANNEL='{channel}' bash"
        )
    }
}

/// Build a reinstall hint for a known installer type.
fn reinstall_hint(installer: &str, channel: &str) -> String {
    match installer {
        "npm" => "Please reinstall via npm:\n  npm i -g @echo-agent-official/echoagent".to_string(),
        "gh-release" => "Please reinstall via GitHub Releases:\n  gh release download --repo fuyuxiang/echo-agent-desktop --pattern 'echo-agent-*' --output echoagent && chmod +x echoagent".to_string(),
        _ => format!("Please reinstall via:\n  {}", manual_install_cmd(channel)),
    }
}

/// True when this process is an x86_64 build translated by Rosetta on an
/// Apple Silicon host. `hw.optional.arm64` is 1 on Apple Silicon — including
/// from a translated process, where the compile-time arch says x86_64.
///
/// Read in-process via `sysctlbyname`: no spawn, no stdout parse, and no
/// dependence on the `sysctl` binary being on PATH. A missing key (genuine
/// Intel Mac) or any error means not Apple Silicon — the probe fails open
/// to the compile-time arch. Cached: fixed host property, read from async
/// paths via [`detect_platform`].
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn running_under_rosetta_on_apple_silicon() -> bool {
    static ROSETTA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ROSETTA.get_or_init(|| {
        let mut val: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        // SAFETY: the name is a valid NUL-terminated C string; `val`/`len`
        // describe a properly sized c_int; sysctlbyname writes at most
        // `len` bytes into `val`.
        let rc = unsafe {
            libc::sysctlbyname(
                c"hw.optional.arm64".as_ptr(),
                (&raw mut val).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        rc == 0 && val == 1
    })
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn running_under_rosetta_on_apple_silicon() -> bool {
    false
}

/// Arch to download artifacts for, given the compile-time arch and whether
/// the host is Apple Silicon running this build under Rosetta. Separated
/// from [`detect_platform`] so the decision is unit-testable.
fn corrected_arch(
    os: &'static str,
    arch: &'static str,
    rosetta_on_apple_silicon: bool,
) -> &'static str {
    if os == "macos" && arch == "x86_64" && rosetta_on_apple_silicon {
        "aarch64"
    } else {
        arch
    }
}

/// Artifact platform from [`detect_platform`]; compile-time values for
/// combos the updater does not support.
fn platform_label() -> String {
    detect_platform()
        .map(|(os, arch)| format!("{os}-{arch}"))
        .unwrap_or_else(|_| format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))
}

/// Typed phase marker for telemetry classification. Deliberately no
/// `source()`, so anyhow's `{:#}` does not print the chain twice.
#[derive(Debug, thiserror::Error)]
enum InstallPhaseError {
    #[error("{0:#}")]
    Download(anyhow::Error),
    #[error("{0:#}")]
    Activate(anyhow::Error),
}

/// Smoke failures stay unwrapped — already typed, and the base-retry abort
/// in [`install_internal_from_bases`] must still downcast them.
fn wrap_download_err(e: anyhow::Error) -> anyhow::Error {
    if e.is::<SmokeTestFailure>() {
        e
    } else {
        InstallPhaseError::Download(e).into()
    }
}

#[doc(hidden)]
pub fn classify_install_error(err: &anyhow::Error) -> CliUpdateErrorKind {
    if let Some(smoke) = err.downcast_ref::<SmokeTestFailure>() {
        return match smoke {
            SmokeTestFailure::Timeout => CliUpdateErrorKind::SmokeTimeout,
            SmokeTestFailure::Spawn(_) => CliUpdateErrorKind::SmokeSpawn,
            SmokeTestFailure::NonZero { .. } => CliUpdateErrorKind::SmokeNonzero,
        };
    }
    match err.downcast_ref::<InstallPhaseError>() {
        Some(InstallPhaseError::Download(_)) => CliUpdateErrorKind::Download,
        Some(InstallPhaseError::Activate(_)) => CliUpdateErrorKind::Activate,
        // npm / gh-release failures carry no phase marker.
        None => CliUpdateErrorKind::Other,
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub installer: Option<String>,
    pub channel: String,
    pub auto_update: Option<bool>,
    pub error: Option<String>,
}

/// Format and print an [`UpdateStatus`] to stdout.
pub fn print_update_status(status: &UpdateStatus, json: bool) -> anyhow::Result<()> {
    if json {
        let payload = serde_json::to_string(status)?;
        println!("{payload}");
        return Ok(());
    }

    if let Some(error) = status.error.as_deref() {
        println!(
            "EchoAgent Build - v{} [{}]",
            status.current_version, status.channel
        );
        println!("Update check failed: {error}");
        return Ok(());
    }

    let channel_label = format!(" [{}]", status.channel);

    if status.update_available {
        if let Some(latest_version) = status.latest_version.as_deref() {
            println!(
                "A new version of EchoAgent Build is available: {} -> {}{}",
                status.current_version, latest_version, channel_label
            );
        } else {
            println!("A new version of EchoAgent Build is available.");
        }
        return Ok(());
    }

    if let Some(latest_version) = status.latest_version.as_deref() {
        println!(
            "EchoAgent Build - v{} (latest: {}){}",
            status.current_version, latest_version, channel_label
        );
        return Ok(());
    }

    println!(
        "EchoAgent Build - v{}{}",
        status.current_version, channel_label
    );
    Ok(())
}

pub async fn check_update_status(update_config: &UpdateConfig) -> UpdateStatus {
    let installer = get_installer().await.map(|value| value.to_string());
    let current_version = get_installed_echo_agent_version();
    let current_config = config::load_config().await;
    let auto_update = current_config.cli.auto_update;
    let channel = update_config.channel.clone();

    let Some(ref inst) = installer else {
        return UpdateStatus {
            current_version,
            latest_version: None,
            update_available: false,
            installer,
            channel,
            auto_update,
            error: None,
        };
    };

    match get_latest_version(inst, update_config).await {
        // --check shares the updater's decision, so it never advertises a version
        // the policy would skip, clamp away, or can't satisfy.
        Ok(latest) => match plan_for(&config::VersionPolicy::resolve(), latest) {
            UpdatePlan::Install { target, .. } => {
                let mut error = None;
                let update_available = match needs_update(
                    &current_version,
                    &target,
                    &channel,
                    false,
                ) {
                    Some(value) => value,
                    None => {
                        // Distinguish parse failure from unsupported channel.
                        let parse_ok = semver::Version::parse(&current_version).is_ok()
                            && semver::Version::parse(&target).is_ok();
                        error = Some(if parse_ok {
                            format!(
                                "Unsupported release channel '{channel}' (current={current_version}, latest={target}). \
                                     Supported channels: stable, alpha, enterprise."
                            )
                        } else {
                            format!(
                                "Failed to parse versions (current={current_version}, latest={target})"
                            )
                        });
                        false
                    }
                };
                UpdateStatus {
                    current_version,
                    latest_version: Some(target),
                    update_available,
                    installer,
                    channel,
                    auto_update,
                    error,
                }
            }
            // Policy skips (anti-downgrade) or can't satisfy the floor: no upgrade.
            UpdatePlan::Skip { latest } | UpdatePlan::Unavailable { latest, .. } => UpdateStatus {
                current_version,
                latest_version: Some(latest),
                update_available: false,
                installer,
                channel,
                auto_update,
                error: None,
            },
        },
        Err(err) => UpdateStatus {
            current_version,
            latest_version: None,
            update_available: false,
            installer,
            channel,
            auto_update,
            error: Some(err.to_string()),
        },
    }
}

enum UpdatePlan {
    /// Anti-downgrade skip; `latest` is reported to the user.
    Skip {
        latest: String,
    },
    /// A hard `required_minimum` exceeds the latest release, so nothing satisfies it.
    Unavailable {
        latest: String,
        target: String,
    },
    Install {
        latest: String,
        target: String,
    },
}

/// Classify a fetched `latest` release under `policy`. Pure; `fetch_update_plan`
/// is the IO wrapper. `--check` shares this so it can't diverge from the updater.
fn plan_for(policy: &config::VersionPolicy, latest: String) -> UpdatePlan {
    let Some(target) = policy.resolve_target(&latest) else {
        return UpdatePlan::Skip { latest };
    };
    // A hard `required_minimum` can clamp above the latest release; that version
    // doesn't exist.
    if matches!(
        (semver::Version::parse(&target), semver::Version::parse(&latest)),
        (Ok(t), Ok(l)) if t > l
    ) {
        UpdatePlan::Unavailable { latest, target }
    } else {
        UpdatePlan::Install { latest, target }
    }
}

async fn fetch_update_plan(
    installer: &str,
    update_config: &UpdateConfig,
    policy: &config::VersionPolicy,
) -> Result<UpdatePlan> {
    let latest = fetch_latest_version(installer, update_config).await?;
    Ok(plan_for(policy, latest))
}

/// Installer + version the leader/background path should converge to: an
/// upgrade OR an authoritative-installer rollback. `None` means stay put. Gates
/// on the installer (via `installer_allows_downgrade`) so npm is never
/// downgraded — the decision depends on the installer, never the caller.
pub async fn auto_update_target(update_config: &UpdateConfig) -> Option<(&'static str, String)> {
    let installer = get_installer().await?;
    let current = get_installed_echo_agent_version();
    let policy = config::VersionPolicy::resolve();
    let UpdatePlan::Install { target, .. } = fetch_update_plan(installer, update_config, &policy)
        .await
        .ok()?
    else {
        return None;
    };
    needs_update(
        &current,
        &target,
        &update_config.channel,
        installer_allows_downgrade(installer),
    )
    .unwrap_or(false)
    .then_some((installer, target))
}

/// Outcome of [`ensure_latest_on_disk`].
#[derive(Debug)]
pub struct EnsureLatestOutcome {
    /// Version this call downloaded and installed; `None` when the disk was
    /// already current (or there was no installer).
    pub installed: Option<String>,
    /// The running process differs from what is now on disk in the channel's
    /// update direction — the caller should relaunch onto the on-disk binary.
    pub relaunch_needed: bool,
}

/// One leader auto-update pass: converge the on-disk install to the channel
/// pointer (downloading **only** when the disk is actually behind it), then
/// report whether the running process should relaunch onto the on-disk binary.
///
/// Unlike [`run_update`] this never uses the compiled-in version for the
/// download decision — a binary already installed by another process (TUI
/// background download, explicit `echoagent update`) is reused as-is. This both
/// removes the duplicate download in leader mode and stops the pre-fix
/// hourly re-download while a busy leader keeps deferring its relaunch.
///
/// When the disk version is unknowable ([`disk_version_for_installer`]:
/// npm-managed installs, Windows copy-based installs, dev builds), this
/// degrades to the pre-fix behavior — download when the *running* process is
/// stale, relaunch only after a download this pass actually installed
/// something. Note the Windows consequence: the hourly busy-leader
/// re-download is NOT fixed there; only the symlink layout can prove the
/// disk is current without exec'ing the binary.
pub async fn ensure_latest_on_disk(update_config: &UpdateConfig) -> Result<EnsureLatestOutcome> {
    let mut outcome = EnsureLatestOutcome {
        installed: None,
        relaunch_needed: false,
    };
    let Some(installer) = get_installer().await else {
        return Ok(outcome);
    };
    heal_managed_install(installer).await;
    let allow_downgrade = installer_allows_downgrade(installer);
    let policy = config::VersionPolicy::resolve();
    let UpdatePlan::Install { target, .. } =
        fetch_update_plan(installer, update_config, &policy).await?
    else {
        return Ok(outcome);
    };

    let effective_current =
        disk_version_for_installer(installer).unwrap_or_else(get_installed_echo_agent_version);
    if needs_update(
        &effective_current,
        &target,
        &update_config.channel,
        allow_downgrade,
    )
    .unwrap_or(false)
    {
        run_install_script(
            installer,
            Some(&target),
            update_config,
            CliUpdateTrigger::LeaderConverge,
        )
        .await?;
        // The leader relaunches right after a successful converge and would
        // die with the event still in flight (failures keep it alive, so
        // successes would under-report). The install is already done.
        echo_agent_telemetry::session_ctx::drain_pending(
            echo_agent_telemetry::session_ctx::CLI_DRAIN,
        )
        .await;
        outcome.installed = Some(target.clone());
    }

    // Relaunch when the running binary differs from what's on disk in the
    // channel's update direction — covers binaries installed by other
    // processes, not just the install above.
    let running = get_installed_echo_agent_version();
    if let Some(disk_now) =
        disk_version_for_installer(installer).or_else(|| outcome.installed.clone())
    {
        outcome.relaunch_needed =
            needs_update(&running, &disk_now, &update_config.channel, allow_downgrade)
                .unwrap_or(false);
    }
    Ok(outcome)
}

/// Disk-version probe gated on the installer actually maintaining the
/// managed `~/.echo-agent/bin/echoagent` symlink.
///
/// Only the internal (install.sh / CDN) and gh-release installers write that
/// symlink. npm manages its own global install, so for npm a symlink left
/// over from a previous internal install would LIE about the npm install's
/// version — in the worst direction, a leftover symlink "newer" than the npm
/// registry would make every updater report "already up to date" and
/// silently suppress npm updates forever. Unknown installers are treated
/// like npm (no trustworthy disk version).
fn disk_version_for_installer(installer: &str) -> Option<String> {
    match installer {
        "internal" | "gh-release" => crate::version::installed_on_disk_version(),
        _ => None,
    }
}

fn env_installer() -> Option<&'static str> {
    if let Ok(v) = std::env::var("ECHO_AGENT_INSTALLER") {
        return match v.to_ascii_lowercase().as_str() {
            "npm" => Some("npm"),
            "internal" => Some("internal"),
            "gh-release" | "gh" => Some("gh-release"),
            _ => None,
        };
    }
    if std::env::var_os("ECHO_AGENT_MANAGED_BY_NPM").is_some() {
        return Some("npm");
    }
    if std::env::var_os("ECHO_AGENT_MANAGED_BY_INTERNAL").is_some() {
        return Some("internal");
    }
    if std::env::var_os("npm_config_user_agent").is_some() {
        return Some("npm");
    }
    None
}

pub async fn get_installer() -> Option<&'static str> {
    if let Some(i) = env_installer() {
        return Some(i);
    }
    let cfg = config::load_config().await;
    match cfg.cli.installer.as_deref() {
        Some("npm") => Some("npm"),
        Some("gh-release") => Some("gh-release"),
        _ => Some("internal"),
    }
}

fn needs_update(current: &str, target: &str, channel: &str, allow_downgrade: bool) -> Option<bool> {
    let current = semver::Version::parse(current).ok()?;
    let target = semver::Version::parse(target).ok()?;
    match channel {
        // NOTE: With the 0.2.X versioning scheme, all versions are plain
        // semver (no pre-release suffix). The pre-release checks in this
        // match are dead code but kept as a safety net.
        "stable" | "enterprise" => {
            if !target.pre.is_empty() {
                tracing::warn!(
                    %current, %target,
                    channel = %channel,
                    "stable/enterprise channel received pre-release candidate, rejecting"
                );
                return Some(false);
            }
            if !current.pre.is_empty() {
                return Some(true);
            }
        }
        "alpha" => {}
        _ => return None,
    }
    Some(if allow_downgrade {
        target != current
    } else {
        target > current
    })
}

/// Returns `true` for installer backends whose version source is authoritative
/// (managed by EchoAgent directly), meaning a pointer rollback is intentional and
/// should trigger a client downgrade. Returns `false` for backends like npm
/// where stale corporate registries/proxies can return arbitrarily old versions.
///
/// Users who installed via `install.sh` are classified as `"internal"` by
/// `get_installer()`, so they also get rollback support.
fn installer_allows_downgrade(installer: &str) -> bool {
    match installer {
        "internal" | "gh-release" => true,
        "npm" => false,
        _ => false,
    }
}

/// Result of a background update availability check.
#[derive(Debug, Clone)]
pub struct UpdateAvailable {
    /// The latest version string (e.g. "0.1.200").
    pub latest_version: String,
}

/// Outcome of [`check_update_background`].
pub struct BackgroundUpdateCheck {
    /// `Some` when the *running* binary is older than the channel pointer —
    /// drives the in-TUI restart hint regardless of who downloads the binary.
    pub update: Option<UpdateAvailable>,
    /// Handle to the background `echoagent update` child, `Some` only when a
    /// download was actually started (the on-disk install was behind the
    /// pointer). The TUI parks this and `wait()`s on it at quit-for-update
    /// time instead of spawning a second downloader.
    pub download: Option<tokio::process::Child>,
}

impl BackgroundUpdateCheck {
    fn none() -> Self {
        Self {
            update: None,
            download: None,
        }
    }
}

/// Check for available updates without blocking the TUI startup.
///
/// Sets [`BackgroundUpdateCheck::update`] when the running binary is older
/// than the channel pointer. If `auto_update` is enabled **and the on-disk
/// install is also behind the pointer**, kicks off a non-blocking download
/// (spawns `echoagent update` as a detached child process) so the new binary is
/// ready when the user quits and relaunches. When another process (an earlier
/// TUI, the leader's hourly checker) already put the target version on disk,
/// no download is started — only the restart hint is surfaced.
pub async fn check_update_background(update_config: &UpdateConfig) -> BackgroundUpdateCheck {
    let Some(installer) = get_installer().await else {
        return BackgroundUpdateCheck::none();
    };

    heal_managed_install(installer).await;

    if is_version_cache_fresh().await {
        return BackgroundUpdateCheck::none();
    }

    let current_config = config::load_config().await;
    if !current_config.cli.auto_update.unwrap_or(false) {
        if current_config.cli.auto_update.is_none()
            && let Err(e) = config::update_config(|st| {
                if st.cli.auto_update.is_none() {
                    st.cli.auto_update = Some(false);
                }
            })
            .await
        {
            tracing::warn!("Failed to save auto-update setting: {}", e);
        }
        return BackgroundUpdateCheck::none();
    }

    let current_version = get_installed_echo_agent_version();
    let policy = config::VersionPolicy::resolve();
    let target_version = match fetch_update_plan(installer, update_config, &policy).await {
        Ok(UpdatePlan::Install { target, .. }) => target,
        Ok(UpdatePlan::Skip { .. } | UpdatePlan::Unavailable { .. }) | Err(_) => {
            return BackgroundUpdateCheck::none();
        }
    };

    let allow_downgrade = installer_allows_downgrade(installer);
    if !needs_update(
        &current_version,
        &target_version,
        &update_config.channel,
        allow_downgrade,
    )
    .unwrap_or(false)
    {
        let stable_ptr = try_fetch_stable_pointer().await;
        write_version_cache(&target_version, stable_ptr.as_deref()).await;
        return BackgroundUpdateCheck::none();
    }

    // Only download when the on-disk install is behind the pointer; the
    // running process being stale (checked above) just means "show the
    // restart hint". The quit-for-update path's `echoagent update` child resolves
    // to "Already up to date" against the same disk state. Gated on the
    // installer maintaining the managed symlink — for npm a leftover symlink
    // would wrongly suppress the download (see `disk_version_for_installer`).
    let disk_needs_download = match disk_version_for_installer(installer) {
        Some(disk) => needs_update(
            &disk,
            &target_version,
            &update_config.channel,
            allow_downgrade,
        )
        .unwrap_or(true),
        None => true,
    };

    // Kick off a non-blocking download so the binary is ready when the
    // user restarts (or accepts the in-TUI restart prompt).
    let download = if disk_needs_download {
        match run_update_subcommand(UpdateRunMode::NonBlocking, CliUpdateTrigger::AutoBackground)
            .await
        {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!("Background update download failed to start: {e}");
                None
            }
        }
    } else {
        tracing::info!(
            target_version = %target_version,
            "Background update: target already on disk, skipping download"
        );
        None
    };

    BackgroundUpdateCheck {
        update: Some(UpdateAvailable {
            latest_version: target_version,
        }),
        download,
    }
}

/// Returns Ok(true) if a blocking update ran; otherwise Ok(false).
pub async fn run_update_if_available(
    run_mode: UpdateRunMode,
    interactive: bool,
    trigger: CliUpdateTrigger,
    update_config: &UpdateConfig,
) -> Result<bool> {
    let Some(inst) = get_installer().await else {
        // Skip update check if no known installer.
        return Ok(false);
    };

    heal_managed_install(inst).await;

    if is_version_cache_fresh().await {
        return Ok(false);
    }

    let current_config = config::load_config().await;

    // The embedded runtime has no standalone release channel. Keep automatic
    // updates opt-in so EchoAgent Desktop remains the single update authority.
    let auto_update = current_config.cli.auto_update.unwrap_or(false);
    if !auto_update {
        if current_config.cli.auto_update.is_none()
            && let Err(e) = config::update_config(|st| {
                if st.cli.auto_update.is_none() {
                    st.cli.auto_update = Some(false);
                }
            })
            .await
        {
            tracing::warn!("Failed to save auto-update setting: {}", e);
        }
        return Ok(false);
    }

    let current_version = get_installed_echo_agent_version();
    let policy = config::VersionPolicy::resolve();
    // Don't write version.json here; only cache after confirming no update is
    // needed or after a successful install, so a failed background download
    // doesn't suppress retries for the TTL window.
    let latest_version = match fetch_update_plan(inst, update_config, &policy).await {
        Ok(UpdatePlan::Install { target, .. }) => target,
        Ok(UpdatePlan::Skip { .. } | UpdatePlan::Unavailable { .. }) | Err(_) => return Ok(false),
    };
    if !needs_update(
        &current_version,
        &latest_version,
        &update_config.channel,
        installer_allows_downgrade(inst),
    )
    .unwrap_or(false)
    {
        let stable_ptr = try_fetch_stable_pointer().await;
        write_version_cache(&latest_version, stable_ptr.as_deref()).await;
        return Ok(false);
    }

    let channel_label = format!(" [{}]", update_config.channel);
    if auto_update {
        eprintln!(
            "A new version of EchoAgent Build is available: {} -> {}{}",
            current_version, latest_version, channel_label
        );
        if interactive {
            if let Err(e) = run_update_subcommand(run_mode, trigger).await {
                eprintln!("Update failed: {}", e);
            } else if matches!(run_mode, UpdateRunMode::Blocking) {
                return Ok(true);
            } else {
                eprintln!("{}", MSG_AUTO_UPDATE_BACKGROUND);
                return Ok(false);
            }
        } else if let Err(e) = run_update_subcommand(run_mode, trigger).await {
            eprintln!("Update failed: {}", e);
        } else if matches!(run_mode, UpdateRunMode::Blocking) {
            return Ok(true);
        }
        return Ok(false);
    } else {
        if current_config
            .cli
            .dismissed_version
            .as_deref()
            .is_some_and(|v| v == latest_version)
        {
            return Ok(false);
        }
        eprintln!(
            "A new version of EchoAgent Build is available: {} -> {}{}",
            current_version, latest_version, channel_label
        );
        if interactive {
            eprintln!("{}", PROMPT_UPDATE_NOW);
            let mut line = String::new();
            if io::stdin().read_line(&mut line).is_ok() {
                let ans = line.trim().to_ascii_lowercase();
                if ans.is_empty() || ans == "y" || ans == "yes" {
                    // Accepted prompt = consent, whatever the caller was.
                    if let Err(e) =
                        run_update_subcommand(run_mode, CliUpdateTrigger::UserCommand).await
                    {
                        eprintln!("Update failed: {}", e);
                    } else if matches!(run_mode, UpdateRunMode::Blocking) {
                        return Ok(true);
                    } else {
                        eprintln!("{}", MSG_AUTO_UPDATE_BACKGROUND);
                        return Ok(false);
                    }
                } else if ans == "d" || ans == "dismiss" {
                    let dismissed = latest_version.clone();
                    if let Err(e) = config::update_config(|st| {
                        st.cli.dismissed_version = Some(dismissed);
                    })
                    .await
                    {
                        tracing::warn!("Failed to save dismissed version: {}", e);
                    }
                }
            }
        } else {
            eprintln!("{}", MSG_RUN_UPDATE_MANUAL);
        }
    }
    Ok(false)
}

/// Launch "echoagent update" in blocking or non-blocking mode.
///
/// In `NonBlocking` mode the spawned child's handle is returned so the caller
/// can later `wait()` on the in-flight download (e.g. the TUI's
/// quit-for-update path) instead of blind-spawning a second downloader.
/// Dropping the handle does not kill the child (`kill_on_drop` is off), so
/// callers that don't care can ignore it. `Blocking` mode returns `None`.
async fn run_update_subcommand(
    run_mode: UpdateRunMode,
    trigger: CliUpdateTrigger,
) -> Result<Option<tokio::process::Child>> {
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(exe);
    // One trigger representation end to end: the enum crosses the process
    // boundary as --trigger=<value> (FromStr on the other side).
    cmd.arg("update");
    cmd.arg(format!("--trigger={}", trigger.as_str()));
    // Hand the resolved telemetry mode to the child, which cannot see the
    // remote-settings layer (requirement pins still beat env). None at the
    // startup spawns — they run before the settings prefetch, when this
    // process knows no more than the child; waiting would let telemetry
    // delay an update.
    if let Some(mode) = echo_agent_telemetry::client::current_mode() {
        cmd.env("ECHO_AGENT_TELEMETRY_ENABLED", mode.to_string());
    }
    match run_mode {
        UpdateRunMode::Blocking => {
            // stderr must be null, not piped: `.status()` does not drain
            // pipes, so if the child writes more than the OS pipe buffer
            // (~16 KB macOS / ~64 KB Linux) to stderr (e.g. download
            // progress bars), the child blocks on the write while the
            // parent blocks on waitpid — deadlocking both processes.
            // With `panic = "abort"`, the blocked child eventually
            // receives SIGABRT.
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                // inherit, not piped: the TUI is already restored so the
                // parent's stderr fd is a normal terminal. inherit lets
                // the child's diagnostic output reach the user. piped +
                // status() would immediately close the read end → EPIPE
                // → panic → SIGABRT (signal 6) under panic=abort.
                .stderr(Stdio::inherit());
            // No detach: the child must stay in the foreground process group so Ctrl+C cancels it with the parent; the atomic install protocol makes mid-download kills safe.
            let status = cmd.status().await?;
            if !status.success() {
                anyhow::bail!("echoagent update failed with {}", status);
            }
            Ok(None)
        }
        UpdateRunMode::NonBlocking => {
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // Detach = new session (Ctrl+C isolation), not handle abandonment:
            // the child is still ours to wait() on.
            echo_agent_tools::util::detach_command(&mut cmd);
            #[allow(clippy::disallowed_methods)] // the caller owns the returned handle
            let child = cmd.spawn()?;
            Ok(Some(child))
        }
    }
}

/// Resolve the echoagent binary path for re-execution after an update.
///
/// `current_exe()` resolves symlinks via `/proc/self/exe` (see proc(5)),
/// so it returns the old versioned target after a symlink swap.
/// Prefer `~/.echo-agent/bin/echoagent` which always points to the latest version.
fn resolve_restart_exe() -> Result<std::path::PathBuf> {
    let canonical = echo_agent_application();
    if canonical.exists() {
        return Ok(canonical);
    }
    Ok(std::env::current_exe()?)
}

/// Restart echoagent with the original command-line arguments to pick up the update.
pub fn restart_echo_agent() -> Result<()> {
    let exe = resolve_restart_exe()?;
    let mut cmd = Command::new(exe);
    for arg in std::env::args_os().skip(1) {
        cmd.arg(arg);
    }
    cmd.env_clear();
    cmd.envs(std::env::vars_os().filter(|(k, _)| k != "ECHO_AGENT_AUTO_UPDATE"));
    eprintln!("Restarting EchoAgent...");

    // Use exec on Unix to replace the current process, avoiding stdio issues
    // when the parent exits. On Windows, fall back to spawn + exit.
    #[cfg(unix)]
    {
        // Flush output before exec to ensure messages are visible
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        let err = cmd.exec();
        // exec only returns if there was an error
        anyhow::bail!("Failed to exec: {}", err);
    }

    #[cfg(not(unix))]
    {
        // Flush output before exit to ensure messages are visible
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        #[allow(clippy::disallowed_methods)] // the relaunched CLI replaces this process
        let _ = cmd.spawn()?;
        std::process::exit(0);
    }
}

pub async fn run_install_script(
    installer: &str,
    target: Option<&str>,
    update_config: &UpdateConfig,
    trigger: CliUpdateTrigger,
) -> Result<()> {
    // What's on disk is being replaced, not this (possibly stale) process's
    // version; npm has no trustworthy disk version, so it falls back.
    let from_version =
        disk_version_for_installer(installer).unwrap_or_else(get_installed_echo_agent_version);
    let started = Instant::now();
    // Internal reports the version it actually activated; npm/gh-release
    // resolve their own artifact, so the requested target stands in.
    let result: Result<Option<String>> = match installer {
        "npm" => install_npm(
            target,
            &update_config.channel,
            update_config.npm_registry.as_deref(),
        )
        .map(|()| None),
        "gh-release" => install_gh_release(target).await.map(|()| None),
        _ => install_internal(target, update_config).await.map(Some),
    };
    // Before the success-only cache sweep, so it cannot inflate successes.
    let duration_ms = started.elapsed().as_millis() as u64;
    if result.is_ok() {
        remove_stale_models_cache().await;
    }
    let (outcome, error_kind) = match &result {
        Ok(_) => (CliUpdateOutcome::Success, None),
        Err(e) => (CliUpdateOutcome::Failed, Some(classify_install_error(e))),
    };
    let to_version = match &result {
        Ok(Some(installed)) => Some(installed.clone()),
        _ => target.map(str::to_string),
    };
    echo_agent_telemetry::session_ctx::log_event(CliUpdate {
        outcome,
        trigger,
        from_version,
        to_version,
        channel: CliUpdateChannel::from_channel_str(&update_config.channel),
        installer: CliUpdateInstaller::from_installer_str(installer),
        platform: platform_label(),
        rosetta: running_under_rosetta_on_apple_silicon(),
        duration_ms,
        error_kind,
    });
    result.map(|_| ()).map_err(|e| {
        anyhow::anyhow!(
            "Auto-update failed: {:#}\n\n{}",
            e,
            reinstall_hint(installer, &update_config.channel)
        )
    })
}

/// Detect the platform (os, arch) to download binaries for.
///
/// Arch is the compile-time arch with one correction: an x86_64 build on an
/// Apple Silicon host (Rosetta) selects `aarch64`, so every update path —
/// interactive `echoagent update`, background `--auto` children, the leader's
/// hourly converge, and forced minimum-version installs — converges to the
/// native build instead of perpetuating the translated one. This mirrors
/// install.sh's `hw.optional.arm64` probe; without it, a lingering x86_64
/// process would reinstall x86_64 right over a fresh native install.
pub(crate) fn detect_platform() -> Result<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        anyhow::bail!("Unsupported OS");
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        anyhow::bail!("Unsupported architecture");
    };
    Ok((
        os,
        corrected_arch(os, arch, running_under_rosetta_on_apple_silicon()),
    ))
}

/// Age past which a leftover `.tmp` download file (or a freshly-renamed
/// versioned binary) is considered abandoned (crashed/killed updater) and
/// safe for `cleanup_old_downloads` to sweep. Generous compared to the
/// longest plausible download (per-request budget is
/// [`DOWNLOAD_REQUEST_TIMEOUT`]; the leader check+download pass matches) so
/// a concurrent updater's in-flight or just-landed file is never deleted
/// out from under it.
const STALE_TMP_AGE: Duration = Duration::from_secs(60 * 60);

/// Total timeout for a CLI artifact download request (including body).
/// Previously 5 minutes, which was too tight on slow links and caused the
/// transfer to abort and restart from zero repeatedly.
const DOWNLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(20 * 60);

fn download_client() -> reqwest::Result<reqwest::Client> {
    echo_agent_extra_ca::build_reqwest_client(|builder| builder.timeout(DOWNLOAD_REQUEST_TIMEOUT))
}

/// Unique temp path for an in-flight download of `dest`.
///
/// Appends `.{pid}-{seq}.tmp` to the FULL file name instead of using
/// `Path::with_extension`, which treats everything after the last dot of the
/// versioned name as the extension (`echo-agent-0.1.181-linux-x86_64` →
/// `echo-agent-0.1.tmp`) and therefore collides for every `0.1.x` version. The PID
/// plus a per-process counter makes the name unique per download attempt —
/// across processes (two updaters racing in the same instant, the accepted
/// lock-free residual race) and within one process — so no racer can ever
/// rename another's half-written temp file into place. Leftovers older than
/// [`STALE_TMP_AGE`] are swept by `cleanup_old_downloads`.
fn tmp_download_path(dest: &std::path::Path) -> std::path::PathBuf {
    unique_temp_sibling(dest, "tmp")
}

/// Unique temp path `<base>.{pid}-{seq}.{ext}`, appended to the full name so a
/// versioned base like `echo-agent-0.1.181` doesn't collide via `with_extension`.
/// PID + per-process counter keep racing updaters from clobbering each other.
fn unique_temp_sibling(base: &std::path::Path, ext: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut name = base
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(
        ".{}-{}.{ext}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    base.with_file_name(name)
}

/// Set `+x` on the temp file before renaming onto `dest`, so a concurrent
/// same-version installer never execs `dest` while it is still 0644.
async fn publish_downloaded_artifact(tmp: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755)).await?;
    }
    tokio::fs::rename(tmp, dest).await?;
    Ok(())
}

/// Files smaller than this are not worth fragmenting across parallel chunks.
const PARALLEL_DOWNLOAD_MIN_BYTES: u64 = 16 * 1024 * 1024;

/// Pick chunk count from file size: 1 chunk per 16 MiB, capped at 8.
fn parallel_chunk_count(size: u64) -> u64 {
    let size_mb = size / (1024 * 1024);
    (size_mb / 16).clamp(1, 8)
}

/// Try a parallel byte-range download to `dest`. Returns Err if the server
/// doesn't advertise a Content-Length, the file is too small to be worth
/// splitting, the range request is rejected, or any chunk transfer fails.
/// The caller is expected to fall back to a single-connection download on Err.
async fn try_parallel_download(
    url: &str,
    dest: &std::path::Path,
    with_progress: bool,
) -> Result<()> {
    let client = download_client()?;

    let head = client.head(url).send().await?;
    if !head.status().is_success() {
        anyhow::bail!("HEAD failed: HTTP {}", head.status());
    }
    let size = head
        .content_length()
        .ok_or_else(|| anyhow::anyhow!("response missing Content-Length"))?;
    if size < PARALLEL_DOWNLOAD_MIN_BYTES {
        anyhow::bail!("file too small for parallel download ({} bytes)", size);
    }

    let n_chunks = parallel_chunk_count(size);
    if n_chunks < 2 {
        anyhow::bail!(
            "file size yields {} chunk(s); not worth parallelizing",
            n_chunks
        );
    }
    let chunk_size = size.div_ceil(n_chunks);

    let pb = if with_progress {
        let pb = ProgressBar::new(size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("  {bar:30.cyan/dim} {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("━╸─"),
        );
        Some(pb)
    } else {
        None
    };

    let tmp = tmp_download_path(dest);
    // Pre-allocate so each task can seek+write to its own range concurrently.
    // One blocking-pool hop instead of two per tokio::fs call.
    let tmp_for_alloc = tmp.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let f = std::fs::File::create(&tmp_for_alloc)?;
        f.set_len(size)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("blocking pre-allocate task panicked: {e}"))??;

    let tasks = (0..n_chunks).map(|i| {
        let start = i * chunk_size;
        let end = std::cmp::min(start + chunk_size, size) - 1;
        let url = url.to_string();
        let tmp = tmp.clone();
        let client = client.clone();
        let pb = pb.clone();
        async move { download_range(&client, &url, &tmp, start, end, pb.as_ref()).await }
    });
    let result = futures::future::try_join_all(tasks).await;

    if let Some(pb) = &pb {
        pb.finish_and_clear();
    }

    match result {
        Ok(_) => {
            publish_downloaded_artifact(&tmp, dest).await?;
            Ok(())
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

/// Fetch bytes `[start, end]` (inclusive) of `url` and write them at `start`
/// in `dest`. Errors if the server doesn't return `206 Partial Content`.
///
/// Streams from the network into a `Vec<u8>` (so progress ticks smoothly as
/// bytes arrive), then issues a single `spawn_blocking` per chunk to do the
/// open + seek + write_all in `std::fs`. This avoids the per-write hop into
/// tokio's blocking pool that `tokio::fs::File::write_all` performs on every
/// ~8 KiB Bytes item from `bytes_stream()`.
async fn download_range(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
    start: u64,
    end: u64,
    progress: Option<&ProgressBar>,
) -> Result<()> {
    let resp = client
        .get(url)
        .header("Range", format!("bytes={}-{}", start, end))
        .send()
        .await?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!("range request rejected: HTTP {}", resp.status());
    }
    let mut buf = Vec::with_capacity((end - start + 1) as usize);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if let Some(pb) = progress {
            pb.inc(chunk.len() as u64);
        }
        buf.extend_from_slice(&chunk);
    }
    let dest = dest.to_owned();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(&dest)?;
        f.seek(SeekFrom::Start(start))?;
        f.write_all(&buf)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("blocking write task panicked: {e}"))??;
    Ok(())
}

/// Download a file from `url` to `dest` with a terminal progress bar.
///
/// If the server provides a `Content-Length` header, a determinate bar is shown
/// with bytes downloaded, total size, and ETA. Otherwise a spinner with a byte
/// counter is used as a fallback.
#[doc(hidden)]
pub async fn download_with_progress(url: &str, dest: &std::path::Path) -> Result<()> {
    // Try parallel byte-range first. Falls through to single-connection on any
    // failure (HEAD missing Content-Length, ranges rejected, partial-fetch error).
    match try_parallel_download(url, dest, true).await {
        Ok(()) => return Ok(()),
        Err(e) => {
            tracing::debug!("parallel download failed, falling back to single connection: {e}")
        }
    }

    let client = download_client()?;
    let resp = client.get(url).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", resp.status());
    }

    let total_size = resp.content_length();

    let pb = if let Some(size) = total_size {
        let pb = ProgressBar::new(size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("  {bar:30.cyan/dim} {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("━╸─"),
        );
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("  {spinner:.cyan} {bytes} downloaded")
                .unwrap(),
        );
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    };

    // Stream to a temp file, then rename atomically
    let tmp = tmp_download_path(dest);
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        pb.inc(chunk.len() as u64);
    }
    file.flush().await?;
    drop(file);

    pb.finish_and_clear();

    publish_downloaded_artifact(&tmp, dest).await?;
    Ok(())
}

/// Download a file silently (no progress bar).
#[doc(hidden)]
pub async fn download_silent(url: &str, dest: &std::path::Path) -> Result<()> {
    match try_parallel_download(url, dest, false).await {
        Ok(()) => return Ok(()),
        Err(e) => {
            tracing::debug!("parallel download failed, falling back to single connection: {e}")
        }
    }

    let client = download_client()?;
    let resp = client.get(url).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", resp.status());
    }

    let tmp = tmp_download_path(dest);
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    publish_downloaded_artifact(&tmp, dest).await?;
    Ok(())
}

/// Delete `~/.echo-agent/models_cache.json` after a successful update.
///
/// The cache embeds the binary version and will be treated as a miss by the
/// new binary anyway, but removing it eagerly avoids a wasted disk read +
/// deserialize on first launch.
async fn remove_stale_models_cache() {
    let cache = echo_agent_home().join("models_cache.json");
    match tokio::fs::remove_file(&cache).await {
        Ok(()) => tracing::debug!("removed stale models_cache.json after update"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::debug!("failed to remove stale models cache: {e}"),
    }
}

/// Remove the stale `echo-agent-pager` symlink/binary from `~/.echo-agent/bin/` left by
/// older installations that shipped a separate pager binary.
async fn remove_stale_pager(bin_dir: &std::path::Path) {
    let name = if cfg!(windows) {
        "echo-agent-pager.exe"
    } else {
        "echo-agent-pager"
    };
    let link = bin_dir.join(name);
    if link.exists() || link.is_symlink() {
        let _ = tokio::fs::remove_file(&link).await;
    }
}

/// Fetch a CLI object from GCS. On Windows the public bucket may use a `.exe`
/// suffix; try that first, then the extensionless name used on macOS/Linux.
async fn download_cli_artifact_from_gcs(
    gcs_base_url: &str,
    object_name: &str,
    dest: &std::path::Path,
    with_progress: bool,
) -> Result<()> {
    let base = gcs_base_url.trim_end_matches('/');
    #[cfg(windows)]
    {
        let with_exe = format!("{}/{}.exe", base, object_name);
        let r = if with_progress {
            download_with_progress(&with_exe, dest).await
        } else {
            download_silent(&with_exe, dest).await
        };
        match r {
            Ok(()) => return Ok(()),
            Err(e) => tracing::debug!("{with_exe} not found, trying extensionless: {e}"),
        }
    }
    let url = format!("{}/{}", base, object_name);
    if with_progress {
        download_with_progress(&url, dest).await
    } else {
        download_silent(&url, dest).await
    }
}

/// Returns the version that was actually activated.
async fn install_internal(target: Option<&str>, update_config: &UpdateConfig) -> Result<String> {
    let bases = crate::version::cli_base_urls();
    let base_refs: Vec<&str> = bases.iter().map(String::as_str).collect();
    install_internal_from_bases(target, update_config, &base_refs).await
}

/// Try the base-dependent install phase ([`download_verified_from_base`]:
/// version resolution, download, smoke test) against each base URL in turn,
/// falling through to the next on any failure. Used to keep installs working
/// when the primary CDN endpoint (Cloudflare) is unreachable but the fallback
/// (direct GCS) still resolves.
///
/// Download-phase side effects (download dir creation, binary fetch) are
/// idempotent, so retrying with a different base after a partial failure is
/// safe. Smoke-test failures ([`SmokeTestFailure`]) are a property of the
/// published artifact, not the CDN — retrying another base will not help.
/// Local activation ([`activate_verified_download`]: link swap, cleanup,
/// config persist) runs once after the first successful download — its
/// failures are not base-dependent, so they abort the install instead of
/// triggering a pointless re-download from the next base.
#[doc(hidden)]
pub async fn install_internal_from_bases(
    target: Option<&str>,
    update_config: &UpdateConfig,
    bases: &[&str],
) -> Result<String> {
    let mut last_err: Option<anyhow::Error> = None;
    for (i, base) in bases.iter().enumerate() {
        match download_verified_from_base(target, update_config, base).await {
            Ok(download) => {
                return activate_verified_download(&download)
                    .await
                    .map(|()| download.version)
                    .map_err(|e| InstallPhaseError::Activate(e).into());
            }
            Err(e) if e.is::<SmokeTestFailure>() => {
                // Same published artifact on every base — retrying will not
                // change a --version timeout or crash. Left unwrapped so
                // telemetry classification sees the typed failure.
                return Err(e);
            }
            Err(e) => {
                let e = wrap_download_err(e);
                if i + 1 < bases.len() {
                    tracing::warn!(
                        "install via {} failed ({:#}); trying next base URL",
                        base,
                        e
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no CLI base URLs to try")))
}

/// First-launch of a freshly downloaded macOS binary can exceed 10s (Rosetta
/// AOT + Gatekeeper on ~140MB). A short cap false-fails a good artifact.
const SMOKE_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Retry budget for exec attempts refused with ETXTBSY. The failure window
/// is normally the microseconds another spawn in this process sits between
/// fork and exec (see [`smoke_test_binary`]), but on a heavily loaded
/// machine that window can stretch, so the budget errs generous — a false
/// "failed to run" both aborts this install and deletes the binary.
const SMOKE_TEST_ETXTBSY_ATTEMPTS: u32 = 8;
const SMOKE_TEST_ETXTBSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(25);

fn truncate_err(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

#[derive(Debug, thiserror::Error)]
enum SmokeTestFailure {
    #[error(
        "downloaded binary failed to run (--version timed out after {}s).\n\
         Your current version is unchanged.",
        SMOKE_TEST_TIMEOUT.as_secs()
    )]
    Timeout,
    #[error(
        "downloaded binary failed to run (could not start: {0}).\n\
         Your current version is unchanged."
    )]
    Spawn(String),
    #[error("{}", nonzero_message(status, stderr))]
    NonZero { status: String, stderr: String },
}

/// `--version` exited nonzero; include stderr only when there is any.
fn nonzero_message(status: &str, stderr: &str) -> String {
    let stderr_line = if stderr.is_empty() {
        String::new()
    } else {
        format!("stderr: {stderr}\n")
    };
    format!(
        "downloaded binary failed to run (--version exited {status}).\n\
         {stderr_line}Your current version is unchanged."
    )
}

async fn smoke_test_binary(binary_path: &std::path::Path) -> Result<(), SmokeTestFailure> {
    // ETXTBSY race: while a concurrent updater in this process is between
    // fork and exec (pre_exec in detach_command forces the fork/exec path),
    // its child briefly holds every open fd — including the write-side fd of
    // a download that has just been renamed onto `binary_path`. Exec'ing a
    // binary whose inode is still open for write fails with ETXTBSY even
    // though the file is complete and healthy, so retry instead of failing
    // the install (and deleting a racer's freshly installed binary).
    let mut last_spawn = String::new();
    for attempt in 1..=SMOKE_TEST_ETXTBSY_ATTEMPTS {
        let mut cmd = tokio::process::Command::new(binary_path);
        cmd.arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        echo_agent_tools::util::detach_command(&mut cmd);
        match tokio::time::timeout(SMOKE_TEST_TIMEOUT, cmd.output()).await {
            Err(_) => return Err(SmokeTestFailure::Timeout),
            Ok(Ok(output)) if output.status.success() => return Ok(()),
            Ok(Ok(output)) => {
                let status = output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| output.status.to_string());
                let stderr = truncate_err(&String::from_utf8_lossy(&output.stderr), 400);
                return Err(SmokeTestFailure::NonZero { status, stderr });
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                last_spawn = e.to_string();
                if attempt < SMOKE_TEST_ETXTBSY_ATTEMPTS {
                    tokio::time::sleep(SMOKE_TEST_ETXTBSY_BACKOFF * attempt).await;
                }
            }
            Ok(Err(e)) => return Err(SmokeTestFailure::Spawn(e.to_string())),
        }
    }
    // Reached only when every attempt hit ETXTBSY; `last_spawn` holds the
    // final spawn error.
    Err(SmokeTestFailure::Spawn(last_spawn))
}

/// Test-only entry point: same as [`install_internal`] but reads from
/// `gcs_base_url` instead of the hardcoded GCS bucket. Persists installer
/// config and writes to `~/.echo-agent/bin/`, so callers must isolate
/// `ECHO_AGENT_HOME`.
#[doc(hidden)]
pub async fn install_internal_from_base(
    target: Option<&str>,
    update_config: &UpdateConfig,
    gcs_base_url: &str,
) -> Result<String> {
    let download = download_verified_from_base(target, update_config, gcs_base_url)
        .await
        .map_err(wrap_download_err)?;
    activate_verified_download(&download)
        .await
        .map(|()| download.version)
        .map_err(|e| InstallPhaseError::Activate(e).into())
}

/// A downloaded and smoke-tested binary in `~/.echo-agent/downloads/`, not yet
/// activated as the managed `echoagent`/`agent`.
struct VerifiedDownload {
    version: String,
    binary_path: std::path::PathBuf,
}

/// Base-dependent install phase: resolve the version (per base when no
/// target is pinned), download the binary, and smoke-test it. Network /
/// fetch failures here are worth retrying against another base URL.
/// [`SmokeTestFailure`] is not — see [`install_internal_from_bases`].
async fn download_verified_from_base(
    target: Option<&str>,
    update_config: &UpdateConfig,
    gcs_base_url: &str,
) -> Result<VerifiedDownload> {
    let (os, arch) = detect_platform()?;
    let platform = format!("{}-{}", os, arch);

    let version = match target {
        Some(v) => {
            semver::Version::parse(v)
                .map_err(|_| anyhow::anyhow!("invalid version format: '{}'", v))?;
            v.to_string()
        }
        None => {
            crate::version::fetch_gcs_version_from_base(&update_config.channel, gcs_base_url)
                .await?
        }
    };

    let echo_agent_home = echo_agent_home();
    let download_dir = echo_agent_home.join("downloads");
    tokio::fs::create_dir_all(&download_dir).await?;

    let binary_name = format!("echo-agent-{}-{}", version, platform);
    let binary_path = download_dir.join(&binary_name);

    eprintln!("  Downloading echoagent v{} ({})...", version, platform);

    // Published already +x (see `publish_downloaded_artifact`).
    download_cli_artifact_from_gcs(gcs_base_url, &binary_name, &binary_path, true).await?;

    // Smoke-test: run the binary before activating it. A truncated or
    // corrupt download is caught here and never becomes the active echoagent.
    if let Err(fail) = smoke_test_binary(&binary_path).await {
        let _ = tokio::fs::remove_file(&binary_path).await;
        // No prefix: run_install_script's wrap adds "Auto-update failed:".
        return Err(fail.into());
    }

    Ok(VerifiedDownload {
        version,
        binary_path,
    })
}

/// Local activation phase: swap the managed bin links to the downloaded
/// binary and finish bookkeeping. Nothing here depends on which base URL
/// served the download, so callers must not retry another base on failure.
async fn activate_verified_download(download: &VerifiedDownload) -> Result<()> {
    let echo_agent_home = echo_agent_home();
    let download_dir = echo_agent_home.join("downloads");
    let bin_dir = echo_agent_home.join("bin");
    tokio::fs::create_dir_all(&bin_dir).await?;

    // Atomic swap of ~/.echo-agent/bin/{echoagent,agent} -> downloaded binary.
    let link_path = swap_managed_bin_links(&download.binary_path, &bin_dir).await?;

    remove_stale_pager(&bin_dir).await;

    eprintln!();

    // Clean up old versioned binaries (keeps current + 1 previous).
    cleanup_old_downloads(&download_dir, "echo-agent", &download.version).await;
    cleanup_old_downloads(&download_dir, "echo-agent-pager", &download.version).await;

    // Persist installer to config.toml so future runs auto-detect internal.
    let _ = config::update_config(|st| {
        st.cli.installer = Some("internal".to_string());
    })
    .await;

    // Regenerate shell completions so they reflect the new binary's CLI surface.
    // Best-effort: failures are silently ignored (same as the installer).
    regenerate_completions(&link_path, &echo_agent_home).await;

    Ok(())
}

/// Regenerate shell completions after a binary update (best-effort).
///
/// Spawns the newly-installed binary with `completions <shell>` for each
/// supported shell and writes the output to the standard completion paths.
/// Failures are silently ignored — completions are a nice-to-have, not a
/// requirement for a successful update.
async fn regenerate_completions(binary: &std::path::Path, echo_agent_home: &std::path::Path) {
    // Derive $HOME independently — echo_agent_home may be overridden via ECHO_AGENT_HOME
    // env var, so echo_agent_home.parent() isn't necessarily the user's home dir.
    #[allow(deprecated)]
    let user_home = std::env::home_dir().unwrap_or_default();

    let completions: &[(&str, std::path::PathBuf)] = &[
        (
            "bash",
            echo_agent_home.join("completions/bash/echoagent.bash"),
        ),
        ("zsh", echo_agent_home.join("completions/zsh/_echo_agent")),
        (
            "fish",
            user_home.join(".config/fish/completions/echoagent.fish"),
        ),
    ];

    for (shell, dest) in completions {
        if let Some(parent) = dest.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let mut cmd = tokio::process::Command::new(binary);
        cmd.args(["completions", shell])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        echo_agent_tools::util::detach_command(&mut cmd);
        let Ok(output) = cmd.output().await else {
            continue;
        };
        if output.status.success() && !output.stdout.is_empty() {
            let _ = tokio::fs::write(dest, &output.stdout).await;
        }
    }
}

/// Compute a relative symlink target from `link` to `target`.
///
/// When both paths share a grandparent (e.g. `~/.echo-agent/bin/echoagent` and
/// `~/.echo-agent/downloads/echo-agent-0.1.203-linux-x86_64`), returns a relative path
/// like `../downloads/echo-agent-0.1.203-linux-x86_64`.  When they share the same
/// parent directory, returns just the filename.  Falls back to the absolute
/// `target` path for any other layout.
///
/// Relative symlinks survive Docker bind-mounts where `~/.echo-agent/` is mapped
/// into a container with a different `$HOME` (and thus a different absolute
/// prefix).
#[cfg(unix)]
fn relative_symlink_target(target: &std::path::Path, link: &std::path::Path) -> std::path::PathBuf {
    let (Some(target_parent), Some(link_parent)) = (target.parent(), link.parent()) else {
        return target.to_path_buf();
    };
    // Same directory — just the filename (e.g. echo-agent-latest -> echo-agent-0.1.203-…)
    if target_parent == link_parent
        && let Some(name) = target.file_name()
    {
        return std::path::PathBuf::from(name);
    }
    // Sibling directories — ../target_dir/filename (e.g. bin/echoagent -> ../downloads/echo-agent-…)
    if let (Some(tp), Some(lp)) = (target_parent.parent(), link_parent.parent())
        && tp == lp
        && let (Some(dir_name), Some(file_name)) = (target_parent.file_name(), target.file_name())
    {
        return std::path::Path::new("..").join(dir_name).join(file_name);
    }
    target.to_path_buf()
}

/// Swap `~/.echo-agent/bin/{echoagent,agent}` to point at `binary_path`. Returns the
/// `echoagent` link path (for [`regenerate_completions`]).
///
/// `echoagent` and `agent` are first-class entry points that the bootstrap
/// installers (`install.sh`, `install.ps1`, `install-enterprise.sh`)
/// maintain in lockstep, and so must the updater — otherwise `echoagent update`
/// leaves `agent` pinned at the previous version.
///
/// Unix: atomic symlink swap with relative target (survives Docker
/// bind-mounts of `~/.echo-agent/`). Windows: [`windows_replace_exe`].
///
/// **All-or-nothing.** Each link's prior state is captured (Unix: prior
/// symlink target; Windows: `.rollback.bak`; or `Absent` marker via
/// `symlink_metadata`) before the swap, and any earlier successful swaps
/// are rolled back if a later one fails — including *removing* a link that
/// didn't exist before. Restore failures go to `tracing::warn!`; the swap
/// error itself propagates unwrapped so the caller's `reinstall_hint` wrap
/// stays the user-visible message.
async fn swap_managed_bin_links(
    binary_path: &std::path::Path,
    bin_dir: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let echo_agent_name = if cfg!(windows) {
        "echoagent.exe"
    } else {
        "echoagent"
    };
    let agent_name = if cfg!(windows) { "agent.exe" } else { "agent" };
    let echo_agent_link = bin_dir.join(echo_agent_name);
    let agent_link = bin_dir.join(agent_name);
    let link_paths: [std::path::PathBuf; 2] = [echo_agent_link.clone(), agent_link];

    // Capture every link up-front so a 2nd-link capture failure can't
    // strand the 1st mid-swap.
    let mut captured: Vec<LinkRollback> = Vec::with_capacity(link_paths.len());
    for path in &link_paths {
        match LinkRollback::capture(path).await {
            Ok(rb) => captured.push(rb),
            Err(e) => {
                // Nothing swapped yet; drop any Windows .rollback.bak files.
                for prior in &captured {
                    prior.cleanup().await;
                }
                return Err(e)
                    .with_context(|| format!("capturing rollback state for {}", path.display()));
            }
        }
    }

    let mut completed: Vec<&LinkRollback> = Vec::with_capacity(captured.len());
    for (i, (link_path, rollback)) in link_paths.iter().zip(captured.iter()).enumerate() {
        #[cfg(unix)]
        let swap_result = {
            let rel_target = relative_symlink_target(binary_path, link_path);
            atomic_symlink_swap(&rel_target, link_path).await
        };
        #[cfg(windows)]
        let swap_result = windows_replace_exe(binary_path, link_path).await;
        #[cfg(not(any(unix, windows)))]
        let swap_result: Result<()> = {
            // No managed bin layout on this target; no-op.
            let _ = (binary_path, link_path);
            Ok(())
        };

        match swap_result {
            Ok(()) => completed.push(rollback),
            Err(e) => {
                // Restore each successful swap in reverse. On restore
                // failure keep the .rollback.bak as a recovery artifact
                // (Windows only) and warn!; the swap error propagates so
                // `reinstall_hint` is the user-visible message.
                for prior in completed.iter().rev() {
                    if let Err(restore_err) = prior.restore().await {
                        let backup_note = prior.backup_path().map_or(String::new(), |p| {
                            format!(" (prior binary preserved at {})", p.display())
                        });
                        tracing::warn!(
                            "failed to roll back managed bin link {}: {restore_err:#}{backup_note}",
                            prior.link_path().display(),
                        );
                        continue;
                    }
                    prior.cleanup().await;
                }
                // Failed swap had no active state to restore; drop its backup.
                rollback.cleanup().await;
                // Drop backups for never-attempted later captures (Windows orphans).
                for later in &captured[i + 1..] {
                    later.cleanup().await;
                }
                return Err(e);
            }
        }
    }

    for cap in &captured {
        cap.cleanup().await;
    }
    Ok(echo_agent_link)
}

/// Snapshot of a managed-bin link's prior state for rollback in
/// [`swap_managed_bin_links`]. `Absent` vs `Present` is discriminated up
/// front via `symlink_metadata` so capture errors never get misread as
/// "link was absent".
enum LinkRollback {
    /// Link was absent before the swap; rollback removes the one we created.
    Absent { link_path: std::path::PathBuf },
    /// Link existed before the swap; rollback restores its prior contents.
    Present {
        link_path: std::path::PathBuf,
        /// Unix: prior symlink target (relative or absolute).
        #[cfg(unix)]
        prior_target: std::path::PathBuf,
        /// Windows: `.rollback.bak` copy of the previous binary.
        #[cfg(windows)]
        backup_path: std::path::PathBuf,
    },
}

impl LinkRollback {
    async fn capture(link_path: &std::path::Path) -> Result<Self> {
        let lp = link_path.to_path_buf();

        // `symlink_metadata` (lstat) handles valid symlinks, broken
        // symlinks, and regular files alike. Any IO error other than
        // NotFound aborts the swap before mutation.
        match tokio::fs::symlink_metadata(&lp).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LinkRollback::Absent { link_path: lp });
            }
            Err(e) => {
                return Err(e).with_context(|| format!("stat {} before swap", lp.display()));
            }
        }

        #[cfg(unix)]
        {
            let prior_target = tokio::fs::read_link(&lp)
                .await
                .with_context(|| format!("reading prior symlink target {}", lp.display()))?;
            Ok(LinkRollback::Present {
                link_path: lp,
                prior_target,
            })
        }
        #[cfg(windows)]
        {
            // Per-process+sequence backup name via `unique_temp_sibling`
            // so concurrent updaters can't clobber each other's backups.
            let backup_path = unique_temp_sibling(&lp, "rollback.bak");
            tokio::fs::copy(&lp, &backup_path).await.with_context(|| {
                format!(
                    "backing up {} to {} before swap",
                    lp.display(),
                    backup_path.display(),
                )
            })?;
            Ok(LinkRollback::Present {
                link_path: lp,
                backup_path,
            })
        }
    }

    fn link_path(&self) -> &std::path::Path {
        match self {
            LinkRollback::Absent { link_path } => link_path,
            LinkRollback::Present { link_path, .. } => link_path,
        }
    }

    /// Path to the on-disk backup (Windows only — Unix is in-memory).
    #[cfg(windows)]
    fn backup_path(&self) -> Option<&std::path::Path> {
        match self {
            LinkRollback::Present { backup_path, .. } => Some(backup_path),
            LinkRollback::Absent { .. } => None,
        }
    }
    #[cfg(unix)]
    fn backup_path(&self) -> Option<&std::path::Path> {
        None
    }

    async fn restore(&self) -> Result<()> {
        match self {
            LinkRollback::Absent { link_path } => {
                // Remove the link we created. NotFound (someone else
                // cleaned up) is fine; anything else is a real failure.
                match tokio::fs::remove_file(link_path).await {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e).with_context(|| {
                        format!("removing rolled-back link {}", link_path.display())
                    }),
                }
            }
            #[cfg(unix)]
            LinkRollback::Present {
                link_path,
                prior_target,
            } => atomic_symlink_swap(prior_target, link_path)
                .await
                .with_context(|| {
                    format!("restoring prior symlink target for {}", link_path.display())
                }),
            #[cfg(windows)]
            LinkRollback::Present {
                link_path,
                backup_path,
            } => {
                // Route through `windows_replace_exe` so rollback inherits
                // the same ERROR_SHARING_VIOLATION rename-aside fallback
                // as the forward path.
                windows_replace_exe(backup_path, link_path)
                    .await
                    .with_context(|| {
                        format!(
                            "restoring {} from {}",
                            link_path.display(),
                            backup_path.display()
                        )
                    })
            }
        }
    }

    async fn cleanup(&self) {
        #[cfg(windows)]
        if let LinkRollback::Present { backup_path, .. } = self {
            let _ = tokio::fs::remove_file(backup_path).await;
        }
        #[cfg(unix)]
        let _ = self; // no on-disk backup on Unix
    }
}

/// Atomically swap a symlink to point to a new target.
///
/// Creates a temporary symlink next to `link_path`, then renames it over the
/// old symlink.  This avoids the remove-then-create race where the path
/// briefly doesn't exist, and — crucially — never deletes the old target
/// file.  On macOS (especially Apple Silicon), deleting a binary that a
/// running process has mmap'd causes SIGKILL because the kernel can no longer
/// verify the code signature of the executable pages.
#[cfg(unix)]
async fn atomic_symlink_swap(target: &std::path::Path, link_path: &std::path::Path) -> Result<()> {
    // Per-racer temp name: a shared one makes remove_file → symlink racy
    // (EEXIST, or ENOENT when another racer renames the link away).
    sweep_stale_tmp_links(link_path, STALE_TMP_AGE).await;
    let tmp_link = unique_temp_sibling(link_path, "tmp-link");
    let _ = tokio::fs::remove_file(&tmp_link).await;
    tokio::fs::symlink(target, &tmp_link).await?;
    tokio::fs::rename(&tmp_link, link_path).await?;
    Ok(())
}

/// Remove `<link>.*.tmp-link` siblings left by a swap that crashed between
/// symlink and rename. Only those older than `max_age` are removed, so a
/// concurrent racer's in-flight link is never deleted out from under it.
#[cfg(unix)]
async fn sweep_stale_tmp_links(link_path: &std::path::Path, max_age: Duration) {
    let (Some(dir), Some(name)) = (
        link_path.parent(),
        link_path.file_name().and_then(|n| n.to_str()),
    ) else {
        return;
    };
    let prefix = format!("{name}.");
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let fname = entry.file_name();
        let Some(fname) = fname.to_str() else {
            continue;
        };
        if !fname.starts_with(&prefix) || !fname.ends_with(".tmp-link") {
            continue;
        }
        let stale = tokio::fs::symlink_metadata(entry.path())
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if stale {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// Replace an executable that may be locked by a running process (Windows).
///
/// On Windows the kernel prevents writes to a running executable but allows
/// renames. If a direct copy fails with a sharing violation, this renames
/// `dest` aside and copies `src` into the freed path. If the copy then
/// fails, the rename is rolled back to avoid a broken install.
///
/// The aside target is normally `<dest>.old`, but a leftover `.old` can
/// itself still be a running image (the session that was live during the
/// previous update keeps executing the renamed-aside file), and a running
/// image can neither be deleted nor rename-replaced. In that case `dest` is
/// renamed to a unique `<dest>.old.{pid}-{seq}.old` sibling instead, so a
/// locked leftover can never block the update. All `.old` leftovers are
/// swept best-effort at the start of each cycle; still-locked ones survive
/// until a later update runs after those processes exit.
#[cfg(windows)]
async fn windows_replace_exe(src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    let file_name = dest
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("destination has no filename: {}", dest.display()))?
        .to_string_lossy();
    let old = dest.with_file_name(format!("{file_name}.old"));

    sweep_old_exe_backups(&old).await;

    match tokio::fs::copy(src, dest).await {
        Ok(_) => return Ok(()),
        // ERROR_SHARING_VIOLATION (32) / ERROR_ACCESS_DENIED (5): exe is
        // locked by a running process. Fall through to rename-and-replace.
        Err(e) if matches!(e.raw_os_error(), Some(32) | Some(5)) => {
            tracing::debug!("exe locked, falling back to rename: {e}");
        }
        Err(e) => return Err(e.into()),
    }

    // A .old that survived the sweep is locked; renaming onto it would need
    // to delete-replace it and fail, so divert to a guaranteed-free name.
    let old_is_free = matches!(
        tokio::fs::symlink_metadata(&old).await,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    let mut aside = if old_is_free {
        old.clone()
    } else {
        let diverted = unique_temp_sibling(&old, "old");
        tracing::debug!(
            "stale {} is locked; diverting aside to {}",
            old.display(),
            diverted.display()
        );
        diverted
    };

    // Move the locked file aside, then copy the new binary into place.
    let mut rename_result = tokio::fs::rename(dest, &aside).await;
    // Pid reuse can collide a diverted name with a dead updater's
    // still-locked leftover, and a racer can occupy a just-checked-free
    // .old; a fresh unique sibling clears both tails (3 attempts total).
    for _ in 0..2 {
        match &rename_result {
            Err(e) if matches!(e.raw_os_error(), Some(32) | Some(5)) => {
                tracing::debug!(
                    "rename aside to {} failed; retrying with a fresh name: {e}",
                    aside.display()
                );
                aside = unique_temp_sibling(&old, "old");
                rename_result = tokio::fs::rename(dest, &aside).await;
            }
            _ => break,
        }
    }
    rename_result.map_err(|e| {
        anyhow::anyhow!(
            "cannot rename locked executable {}: {e}\n\
             Close all running echoagent sessions and retry.",
            dest.display(),
        )
    })?;
    match tokio::fs::copy(src, dest).await {
        Ok(_) => Ok(()),
        Err(e) => {
            // Rollback: restore the old binary so the install isn't broken.
            let _ = tokio::fs::rename(&aside, dest).await;
            Err(e.into())
        }
    }
}

/// Best-effort removal of `<exe>.old` plus the unique
/// `<exe>.old.{pid}-{seq}.old` asides accumulated by prior update cycles.
/// Locked ones (still-running images) survive and are collected by a later
/// update once those processes exit. The `<exe>.old` prefix keeps the sweep
/// away from `<exe>` itself, other executables' leftovers, and the
/// `.rollback.bak` / `.tmp` sibling shapes.
///
/// Unlike `sweep_stale_tmp_links` there is deliberately no `max_age` gate:
/// rename preserves mtime, so a racer's seconds-old aside already looks
/// days old and age cannot distinguish it; in-use asides survive deletion
/// by being locked; and deleting a racer's fresh unlocked aside (its
/// rollback source while both racers converge on the same dest) is the
/// accepted lock-free residual race (see `tmp_download_path`).
#[cfg(windows)]
async fn sweep_old_exe_backups(old: &std::path::Path) {
    let _ = tokio::fs::remove_file(old).await;
    let (Some(dir), Some(old_name)) = (old.parent(), old.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let prefix = format!("{old_name}.");
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(&prefix) && name.ends_with(".old") {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// Best-effort cleanup of old versioned binaries for a given binary name.
///
/// Mirrors the npm `cleanupOldVersions()` policy: keeps the current version
/// plus one previous version (in case a process is still running the old binary
/// and hasn't fully loaded all pages yet — deleting it on macOS causes SIGKILL
/// because the kernel can no longer verify the code signature).
///
/// `bin_prefix` is the downloaded artifact prefix, e.g. `"echo-agent"` or
/// `"echo-agent-pager"`.
/// Files must match `{bin_prefix}-{digit}*` to be considered versioned binaries
/// (this avoids `echo-agent-*` matching `echo-agent-pager-*` or `echo-agent-latest`).
///
/// Temporary/partial files (containing `.tmp`) are deleted only once they
/// are **stale** (mtime older than [`STALE_TMP_AGE`]). A fresh `.tmp` may be
/// a concurrent updater's in-flight download — the same-instant race the
/// lock-free design accepts — and deleting it out from under that updater
/// would make its atomic rename fail.
async fn cleanup_old_downloads(dir: &std::path::Path, bin_prefix: &str, current_version: &str) {
    let prefix = format!("{}-", bin_prefix);
    let current_semver = match semver::Version::parse(current_version) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "cleanup_old_downloads: invalid current version '{}': {}",
                current_version,
                e
            );
            return;
        }
    };

    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(
                "cleanup_old_downloads: failed to read {}: {}",
                dir.display(),
                e
            );
            return;
        }
    };

    let mut versioned: Vec<(semver::Version, String)> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        // Temp/partial files: sweep only STALE ones. A fresh `.tmp` may be a
        // concurrent updater's in-flight download — deleting it would make
        // that updater's atomic rename fail with ENOENT.
        if name.contains(".tmp") {
            let stale = match entry.metadata().await.and_then(|m| m.modified()) {
                Ok(modified) => std::time::SystemTime::now()
                    .duration_since(modified)
                    .map(|age| age > STALE_TMP_AGE)
                    // Future mtime (clock skew): can't tell — leave it.
                    .unwrap_or(false),
                // Unknown mtime: leave it; it is swept once readable+old.
                Err(_) => false,
            };
            if stale && let Err(e) = tokio::fs::remove_file(entry.path()).await {
                tracing::warn!("failed to remove stale temp file {}: {}", name, e);
            }
            continue;
        }
        // Skip symlinks (e.g. echo-agent-latest).
        if let Ok(ft) = entry.file_type().await
            && ft.is_symlink()
        {
            continue;
        }
        // The suffix after the prefix must start with a digit to be a versioned
        // binary (avoids `echo-agent-latest`, `echo-agent-pager-*` when prefix is
        // `echo-agent`).
        let suffix = &name[prefix.len()..];
        if !suffix.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        // Extract the version portion via the shared parser (handles the
        // internal `echo-agent-0.1.150-macos-aarch64`, pre-release, and npm
        // `echo-agent-0.1.150` layouts — see `version_from_versioned_binary_name`).
        let Some(ver_str) = crate::version::version_from_versioned_binary_name(&name, bin_prefix)
        else {
            continue;
        };
        if let Ok(v) = semver::Version::parse(&ver_str) {
            // Skip the current version — never delete it.
            if v == current_semver {
                continue;
            }
            versioned.push((v, name));
        }
    }

    // Sort descending by version so the newest is first.
    versioned.sort_by(|a, b| b.0.cmp(&a.0));

    // Keep the most recent old version (index 0), delete the rest (index 1+).
    // This matches the npm policy: current + 1 previous.
    for (_, name) in versioned.iter().skip(1) {
        let path = dir.join(name);
        // Same freshness guard as the `.tmp` sweep: a versioned binary
        // written moments ago is likely a concurrent installer's
        // just-renamed download (its symlink swap hasn't happened yet) —
        // deleting it would leave that installer's swap pointing at
        // nothing. Old binaries from previous releases are days old.
        let fresh = tokio::fs::metadata(&path)
            .await
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age <= STALE_TMP_AGE);
        if fresh {
            continue;
        }
        if let Err(e) = tokio::fs::remove_file(&path).await {
            tracing::warn!("failed to remove old binary {}: {}", name, e);
        }
    }
}

fn installer_manages_bin_entrypoints(installer: &str) -> bool {
    matches!(installer, "internal" | "gh-release")
}

#[cfg_attr(not(any(unix, windows)), allow(clippy::unused_async))]
async fn heal_managed_install(installer: &str) {
    if !installer_manages_bin_entrypoints(installer) {
        return;
    }

    #[cfg(any(unix, windows))]
    {
        let bin_dir = echo_agent_home().join("bin");

        #[cfg(unix)]
        reconcile_agent_to_echo_agent(&bin_dir).await;

        #[cfg(windows)]
        reconcile_agent_exe_to_echo_agent(&bin_dir).await;
    }
}

#[cfg(unix)]
async fn reconcile_agent_to_echo_agent(bin_dir: &std::path::Path) {
    let echo_agent_link = bin_dir.join("echoagent");
    let agent_link = bin_dir.join("agent");

    let Ok(echo_agent_target) = tokio::fs::read_link(&echo_agent_link).await else {
        return;
    };
    if tokio::fs::metadata(&echo_agent_link).await.is_err() {
        return;
    }
    if let Ok(agent_target) = tokio::fs::read_link(&agent_link).await
        && agent_target == echo_agent_target
    {
        return;
    }
    match atomic_symlink_swap(&echo_agent_target, &agent_link).await {
        Ok(()) => tracing::info!(
            echo_agent_target = %echo_agent_target.display(),
            "reconciled agent bin symlink to echoagent target"
        ),
        Err(e) => tracing::warn!("failed to reconcile agent bin symlink: {e:#}"),
    }
}

#[cfg(windows)]
async fn reconcile_agent_exe_to_echo_agent(bin_dir: &std::path::Path) {
    let echo_agent_exe = bin_dir.join("echoagent.exe");
    let agent_exe = bin_dir.join("agent.exe");

    if tokio::fs::metadata(&echo_agent_exe).await.is_err() {
        return;
    }
    match agent_exe_differs(&echo_agent_exe, &agent_exe).await {
        Ok(true) => {}
        Ok(false) => return,
        Err(e) => {
            tracing::debug!("agent.exe reconcile: compare failed: {e:#}");
            return;
        }
    }
    match windows_replace_exe(&echo_agent_exe, &agent_exe).await {
        Ok(()) => tracing::info!("reconciled agent.exe to echoagent.exe"),
        Err(e) => tracing::warn!("failed to reconcile agent.exe to echoagent.exe: {e:#}"),
    }
}

#[cfg(windows)]
async fn agent_exe_differs(
    echoagent: &std::path::Path,
    agent: &std::path::Path,
) -> std::io::Result<bool> {
    use tokio::io::{AsyncReadExt, BufReader};
    let echo_agent_len = tokio::fs::metadata(echoagent).await?.len();
    match tokio::fs::metadata(agent).await {
        Ok(m) if m.len() != echo_agent_len => return Ok(true),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e),
    }
    let mut rg = BufReader::new(tokio::fs::File::open(echoagent).await?);
    let mut ra = BufReader::new(tokio::fs::File::open(agent).await?);
    let mut bg = [0u8; 64 * 1024];
    let mut ba = [0u8; 64 * 1024];
    loop {
        let n = rg.read(&mut bg).await?;
        if n == 0 {
            return Ok(false);
        }
        ra.read_exact(&mut ba[..n]).await?;
        if bg[..n] != ba[..n] {
            return Ok(true);
        }
    }
}

/// Download a single asset from a GitHub release via `gh release download`.
async fn gh_release_download(tag: &str, pattern: &str, dest: &std::path::Path) -> Result<()> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} Downloading from GitHub Releases...")
            .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(100));

    let mut cmd = tokio::process::Command::new("gh");
    cmd.args([
        "release",
        "download",
        tag,
        "--repo",
        crate::version::GH_RELEASE_REPO,
        "--pattern",
        pattern,
        "--output",
        &dest.to_string_lossy(),
        "--clobber",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    let output = cmd.output().await?;

    pb.finish_and_clear();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "gh release download failed for {} tag {} from {}: {}",
            pattern,
            tag,
            crate::version::GH_RELEASE_REPO,
            stderr.trim()
        );
    }
    Ok(())
}

/// Download and install echoagent from GitHub Releases (fuyuxiang/echo-agent-desktop).
///
/// Uses `gh release download` to fetch the binary matching the current platform.
/// This works anywhere the `gh` CLI is authenticated, without needing npm or
/// internal network access.
async fn install_gh_release(target: Option<&str>) -> Result<()> {
    let (os, arch) = detect_platform()?;
    let platform = format!("{}-{}", os, arch);

    let version = match target {
        Some(v) => v.to_string(),
        None => crate::version::fetch_gh_release_version("stable").await?,
    };

    let echo_agent_home = echo_agent_home();
    let download_dir = echo_agent_home.join("downloads");
    let bin_dir = echo_agent_home.join("bin");
    tokio::fs::create_dir_all(&download_dir).await?;
    tokio::fs::create_dir_all(&bin_dir).await?;

    let binary_name = format!("echo-agent-{}-{}", version, platform);
    let binary_path = download_dir.join(&binary_name);
    let tag = format!("v{}", version);

    eprintln!(
        "  Downloading echoagent v{} ({}) from GitHub Releases...",
        version, platform
    );

    gh_release_download(&tag, &binary_name, &binary_path).await?;

    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755)).await?;
    }

    // Atomic swap of ~/.echo-agent/bin/{echoagent,agent} -> downloaded binary.
    swap_managed_bin_links(&binary_path, &bin_dir).await?;

    // Update echo-agent-latest -> versioned binary so any existing symlinks that route
    // through it (e.g. /usr/local/bin/echoagent -> ~/.echo-agent/downloads/echo-agent-latest)
    // resolve to the newly installed version.
    #[cfg(unix)]
    {
        let latest_path = download_dir.join("echo-agent-latest");
        let rel_target = relative_symlink_target(&binary_path, &latest_path);
        if let Err(e) = atomic_symlink_swap(&rel_target, &latest_path).await {
            tracing::warn!("Failed to update echo-agent-latest symlink: {e}");
        }
    }

    // Also update /usr/local/bin/{echoagent,agent} if either points directly into
    // ~/.echo-agent/downloads/ (legacy layout — skips the echo-agent-latest indirection).
    // Permission errors ignored.
    #[cfg(unix)]
    for name in ["echoagent", "agent"] {
        let system_link = std::path::PathBuf::from(format!("/usr/local/bin/{name}"));
        if let Ok(existing_target) = tokio::fs::read_link(&system_link).await {
            let target_str = existing_target.to_string_lossy();
            if target_str.contains(".echo-agent/downloads/")
                && !target_str.ends_with("echo-agent-latest")
            {
                // Try to update; ignore permission errors
                let _ = atomic_symlink_swap(&binary_path, &system_link).await;
            }
        }
    }

    remove_stale_pager(&bin_dir).await;

    eprintln!();

    // Clean up old versioned binaries (keeps current + 1 previous).
    cleanup_old_downloads(&download_dir, "echo-agent", &version).await;
    cleanup_old_downloads(&download_dir, "echo-agent-pager", &version).await;

    // Persist installer to config.toml so future runs auto-detect gh-release.
    let _ = config::update_config(|st| {
        st.cli.installer = Some("gh-release".to_string());
    })
    .await;

    Ok(())
}

/// Creates a temporary .npmrc file with the NPM token if present.
/// Returns the path to the created file, or None if no token was set.
fn create_temp_npmrc(npm_registry: Option<&str>) -> Result<Option<std::path::PathBuf>> {
    if let Ok(token) = std::env::var("NPM_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            let dir = std::env::temp_dir();
            let npmrc_path = dir.join(format!(".npmrc-{}-install", std::process::id()));
            let registry_host = npm_registry
                .and_then(|r| reqwest::Url::parse(r).ok())
                .map(|u| {
                    let host = u.host_str().unwrap_or("registry.npmjs.org");
                    let port_suffix = u.port().map(|p| format!(":{}", p)).unwrap_or_default();
                    format!("{}{}{}", host, port_suffix, u.path().trim_end_matches('/'))
                })
                .unwrap_or_else(|| "registry.npmjs.org".to_string());
            let npmrc_content = format!("//{}/:_authToken={}\n", registry_host, token);
            std::fs::write(&npmrc_path, npmrc_content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&npmrc_path, std::fs::Permissions::from_mode(0o600))?;
            }
            return Ok(Some(npmrc_path));
        }
    }
    Ok(None)
}

/// Check if other echoagent processes are running (macOS only).
///
/// On macOS, `npm i -g` replaces the vendored binary in node_modules in-place.
/// Any echoagent process running from that vendored path will be SIGKILL'd by the
/// kernel because macOS (Apple Silicon in particular) can no longer verify
/// the code signature of the mmap'd executable pages once the backing file
/// inode is unlinked.
///
/// While our postinstall.js now uses versioned binaries under ~/.echo-agent/bin/
/// (so processes launched from there are safe), older installations or npx
/// invocations may still be running the vendored binary directly.
#[cfg(target_os = "macos")]
fn warn_if_other_echo_agent_processes_running() {
    let my_pid = std::process::id().to_string();
    let mut cmd = Command::new("pgrep");
    cmd.args(["-f", "echoagent"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    echo_agent_tools::util::detach_std_command(&mut cmd);
    if let Ok(output) = cmd.output() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let other_pids: Vec<&str> = stdout
            .lines()
            .map(|l| l.trim())
            .filter(|pid| !pid.is_empty() && *pid != my_pid)
            .collect();
        if !other_pids.is_empty() {
            eprintln!(
                "  ⚠ Warning: {} other echoagent process(es) detected.",
                other_pids.len()
            );
            eprintln!("    Processes running from the npm vendored binary path may be");
            eprintln!("    killed by macOS when npm replaces the package files.");
            eprintln!("    Consider closing other echoagent sessions before updating.");
            eprintln!();
        }
    }
}

/// Test-only entry point: invokes the private [`install_npm`] for tests
/// that swap in a fake `npm` via PATH.
#[doc(hidden)]
pub fn install_npm_for_test(
    target: Option<&str>,
    channel: &str,
    npm_registry: Option<&str>,
) -> Result<()> {
    install_npm(target, channel, npm_registry)
}

fn install_npm(target: Option<&str>, channel: &str, npm_registry: Option<&str>) -> Result<()> {
    // Warn on macOS about potential impact on other running processes.
    #[cfg(target_os = "macos")]
    warn_if_other_echo_agent_processes_running();

    let version_arg = match target {
        Some(ver) => format!("@echo-agent-official/echoagent@{ver}"),
        None => {
            // All current callers resolve the version via get_latest_version
            // (which applies max(stable, alpha) for the alpha channel) before
            // reaching here.  Falling back to a raw dist-tag would bypass that
            // logic, so warn loudly if this path is ever hit.
            tracing::warn!(
                channel,
                "install_npm called without a resolved version, falling back to dist-tag"
            );
            format!(
                "@echo-agent-official/echoagent@{}",
                if channel == "alpha" {
                    "alpha"
                } else {
                    "latest"
                }
            )
        }
    };

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} Installing via npm...")
            .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(100));

    let mut cmd = Command::new("npm");
    cmd.args(["i", "-g", &version_arg]);
    if let Some(registry) = npm_registry {
        cmd.arg(format!("--registry={}", registry));
    }

    // Use a temporary .npmrc to avoid exposing the token in process lists or shell history.
    let temp_npmrc = create_temp_npmrc(npm_registry)?;
    if let Some(ref npmrc_path) = temp_npmrc {
        cmd.arg(format!("--userconfig={}", npmrc_path.display()));
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        // inherit, not piped — same rationale as run_update_subcommand.
        .stderr(Stdio::inherit());
    echo_agent_tools::util::detach_std_command(&mut cmd);
    let status = cmd.status()?;

    if let Some(path) = temp_npmrc
        && let Err(e) = std::fs::remove_file(&path)
    {
        tracing::warn!("Failed to remove temp .npmrc file: {}", e);
    }

    pb.finish_and_clear();

    if !status.success() {
        anyhow::bail!("npm install failed. Please try again.");
    }
    eprintln!();
    Ok(())
}

pub async fn apply_channel_switch(channel_switch: Option<&str>, update_config: &mut UpdateConfig) {
    if let Some(ch) = channel_switch
        && update_config.channel != ch
    {
        let _ = config::update_config(|st| {
            st.cli.channel = Some(ch.to_string());
        })
        .await;
        update_config.channel = ch.to_string();
        eprintln!("Switched to {} channel.", ch);
    }
}

/// Run the `echoagent update` command. Returns `Ok(Some(version))` when the target
/// version is present on disk afterwards — either installed by this call or
/// found already installed (e.g. by a concurrent background download); returns
/// `Ok(None)` when there is no installer or no applicable target. Callers use
/// the returned version to signal a running leader to relaunch onto the new
/// binary (see the pager's post-update leader relaunch) — that signal must
/// fire even when the download itself was skipped, so a stale leader still
/// picks up a binary someone else installed.
pub async fn run_update(
    force: bool,
    pinned_version: Option<&str>,
    channel_switch: Option<&str>,
    update_config: &mut UpdateConfig,
    trigger: CliUpdateTrigger,
) -> Result<Option<String>> {
    apply_channel_switch(channel_switch, update_config).await;
    let installer = match get_installer().await {
        Some(i) => i,
        None => {
            eprintln!("Auto-update is not available for manual installations.");
            return Ok(None);
        }
    };
    // Persist installer if not already saved
    let cfg = config::load_config().await;
    if cfg.cli.installer.is_none() {
        let _ = config::update_config(|st| {
            st.cli.installer = Some(installer.to_string());
        })
        .await;
    }

    heal_managed_install(installer).await;

    let current_version = get_installed_echo_agent_version();
    let policy = config::VersionPolicy::resolve();

    // When --version is given, skip the latest-version check and install directly
    if let Some(version) = pinned_version {
        if let Err(e) = crate::version_policy::check_install_target(&policy, version) {
            anyhow::bail!("{e}");
        }
        eprintln!(
            "Installing EchoAgent {} (current: {})...",
            version, current_version
        );
        eprintln!();
        run_install_script(installer, Some(version), update_config, trigger).await?;
        refresh_deployment_config().await;
        if let Err(e) = config::update_config(|st| {
            st.cli.auto_update = Some(false);
        })
        .await
        {
            tracing::warn!("Failed to persist auto_update=false for pinned install: {e}");
        }
        eprintln!("  ✓ echoagent v{} installed successfully!", version);
        eprintln!("  Please restart EchoAgent.");
        return Ok(Some(version.to_string()));
    }

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} Checking for updates...")
            .unwrap(),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    let plan = fetch_update_plan(installer, update_config, &policy).await?;
    pb.finish_and_clear();

    let (latest_version, install_target) = match plan {
        UpdatePlan::Skip { latest } => {
            // Cache so an explicit `echoagent update` doesn't re-prompt every run.
            let stable_ptr = try_fetch_stable_pointer().await;
            write_version_cache(&latest, stable_ptr.as_deref()).await;
            eprintln!(
                "The latest release ({latest}) is not an allowed update; \
                 keeping the current version ({current_version})."
            );
            refresh_deployment_config().await;
            return Ok(None);
        }
        UpdatePlan::Unavailable { latest, target } => {
            anyhow::bail!(
                "The required minimum version ({target}) is newer than the latest \
                 available release ({latest}). Contact your administrator."
            );
        }
        UpdatePlan::Install { latest, target } => (latest, target),
    };
    if install_target != latest_version {
        eprintln!(
            "Latest available is {latest_version}, but your configured version range \
             allows {install_target}; installing that instead."
        );
    }

    // What's on disk wins over this process's compiled-in version: a
    // concurrent or earlier updater (TUI background download, leader hourly
    // checker) may already have installed the target, in which case there is
    // nothing to download. Gated on the installer maintaining the managed
    // symlink — for npm a leftover symlink would lie (see
    // `disk_version_for_installer`).
    let effective_current =
        disk_version_for_installer(installer).unwrap_or_else(|| current_version.clone());

    if !force {
        match needs_update(
            &effective_current,
            &install_target,
            &update_config.channel,
            installer_allows_downgrade(installer),
        ) {
            Some(true) => {}
            Some(false) => {
                // Explicit channel switch (--stable / --alpha) with a
                // different target version: install even though the current
                // version is "newer" by semver. This handles switching from
                // alpha 0.2.X back to stable 0.1.220 where 0.2.X > 0.1.220.
                if channel_switch.is_some() && effective_current != install_target {
                    // Fall through to install
                } else {
                    let stable_ptr = try_fetch_stable_pointer().await;
                    write_version_cache(&install_target, stable_ptr.as_deref()).await;
                    eprintln!("Already up to date ({}).", effective_current);
                    // Retry if a prior sync failed.
                    refresh_deployment_config().await;
                    // The target is on disk even though this call installed
                    // nothing — report it so the caller still signals stale
                    // leaders to relaunch onto it (signalling is directional
                    // and skips leaders already at/after this version).
                    return Ok(Some(install_target));
                }
            }
            None => {
                // Distinguish parse failure from unsupported channel.
                let parse_ok = semver::Version::parse(&effective_current).is_ok()
                    && semver::Version::parse(&install_target).is_ok();
                if parse_ok {
                    anyhow::bail!(
                        "Unsupported release channel '{}' (current={}, target={}). \
                         Supported channels: stable, alpha, enterprise. \
                         Use --stable or --alpha to override, or set [cli] channel in config.toml.",
                        update_config.channel,
                        effective_current,
                        install_target
                    );
                } else {
                    anyhow::bail!(
                        "Failed to parse versions (current={}, target={})",
                        effective_current,
                        install_target
                    );
                }
            }
        }
    }

    let target_version = if force
        && !needs_update(
            &effective_current,
            &install_target,
            &update_config.channel,
            installer_allows_downgrade(installer),
        )
        .unwrap_or(true)
    {
        eprintln!(
            "Forcing reinstall of EchoAgent {} (already up to date)",
            effective_current
        );
        &effective_current
    } else {
        eprintln!(
            "Updating EchoAgent {} → {}",
            effective_current, install_target
        );
        &install_target
    };

    eprintln!();
    run_install_script(installer, Some(target_version), update_config, trigger).await?;
    // Fetch the stable pointer now so the new binary has it immediately
    // for channel_label() display, rather than waiting for the next
    // TTL-gated update check (~30 min).
    let stable_ptr = try_fetch_stable_pointer().await;
    write_version_cache(target_version, stable_ptr.as_deref()).await;
    refresh_deployment_config().await;
    eprintln!("  ✓ echoagent v{} installed successfully!", target_version);

    if !force && std::env::var_os("ECHO_AGENT_AUTO_UPDATE").is_none() {
        eprintln!("  Please restart EchoAgent.");
    }
    Ok(Some(target_version.to_string()))
}

/// Refresh managed config post-update (best-effort, staleness-gated), for
/// deployment-key and team principals alike.
async fn refresh_deployment_config() {
    if !echo_agent_runtime::managed_config::has_principal() {
        return;
    }
    if !echo_agent_runtime::managed_config::is_fetch_enabled() {
        return;
    }
    // Clear a logged-out team's files before deciding to fetch (mirrors the loop).
    echo_agent_runtime::managed_config::clear_orphan();
    if !echo_agent_runtime::config::is_managed_config_stale_for(
        &echo_agent_runtime::managed_config::current_serving_identity(),
    ) {
        return;
    }
    match echo_agent_runtime::managed_config::sync().await {
        Ok(true) => eprintln!("  Applied managed configuration."),
        Ok(false) => tracing::debug!("no managed configuration to apply"),
        // Auth issues aren't actionable mid-update: quiet here, loud on `echoagent setup`.
        Err(e) if e.is_auth_rejection() => tracing::debug!("managed config not applied: {e}"),
        Err(e) if e.is_retryable() => {
            tracing::debug!("managed config refresh failed: {e}");
            eprintln!("  Couldn't apply managed configuration. Run `echoagent setup` to retry.");
        }
        Err(e) => eprintln!("  Couldn't apply managed configuration. {e}"),
    }
}

#[cfg(test)]
#[path = "auto_update_tests.rs"]
mod tests;
