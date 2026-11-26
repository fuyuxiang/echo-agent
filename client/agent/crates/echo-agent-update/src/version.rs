use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use tokio::fs;
use tokio::process::Command;

use echo_agent_runtime::env::EchoAgentBuildEnvironment;
use echo_agent_runtime::util::echo_agent_home::echo_agent_home;

const TTL_SECONDS_BEFORE_AUTO_UPDATE: Duration = Duration::from_secs(60 * 30);
const NPM_PACKAGE: &str = "@echo-agent-official/echoagent";
pub const GH_RELEASE_REPO: &str = "fuyuxiang/echo-agent-desktop";

/// Primary CLI base URL: Cloudflare-fronted echo.agent endpoint with edge caching
/// for binaries and origin-respecting no-cache for channel pointers.
pub(crate) const CLI_BASE_URL_PRIMARY: &str = "https://echo-agent.invalid/cli";

/// Fallback CLI base URL: direct GCS, used when the primary is unreachable
/// (Cloudflare outage, regional CF egress issue, DNS hijack, etc.).
pub(crate) const CLI_BASE_URL_FALLBACK: &str = "https://artifacts.echo-agent.invalid/cli";

/// CLI base URLs in preference order. Callers (channel-pointer fetch, binary
/// download, in-app updater) try each in turn and stop at the first success.
pub(crate) const CLI_BASE_URLS: &[&str] = &[CLI_BASE_URL_PRIMARY, CLI_BASE_URL_FALLBACK];

/// [`CLI_BASE_URLS`] with a test seam: `ECHO_AGENT_CLI_BASE_URL` points fetches
/// and downloads at one base (env-seam family of `ECHO_AGENT_INSTALLER`).
/// Loopback-only: downloads are verified by a smoke test, not a checksum,
/// so an arbitrary redirect base would be an install-hijack vector.
pub(crate) fn cli_base_urls() -> Vec<String> {
    if let Ok(base) = std::env::var("ECHO_AGENT_CLI_BASE_URL") {
        let base = base.trim();
        if is_loopback_base(base) {
            return vec![base.to_owned()];
        }
        if !base.is_empty() {
            tracing::warn!("ECHO_AGENT_CLI_BASE_URL ignored: only loopback bases are honored");
        }
    }
    CLI_BASE_URLS.iter().map(|s| (*s).to_owned()).collect()
}

/// Parsed, not prefix-matched: `http://127.0.0.1:9@evil.com` starts with a
/// loopback prefix but its host is `evil.com` (userinfo trick).
fn is_loopback_base(base: &str) -> bool {
    let Ok(u) = url::Url::parse(base) else {
        return false;
    };
    if u.scheme() != "http" || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match u.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d == "localhost",
        None => false,
    }
}

/// Minimal configuration the update system needs from the environment.
///
/// Constructed once from `EchoAgentBuildEnvironment` at startup and threaded through the
/// update call chain so that `auto_update` and `version` never need to know
/// about the `EchoAgentBuildEnvironment` enum directly.
#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// Chat API proxy base URL (versioned `https://chat-proxy.echo-agent.invalid/v1` endpoint).
    pub proxy_base_url: String,
    /// Auth scope key for `~/.echo-agent/auth.json`.
    pub auth_scope: String,
    /// Enterprise deployment key (ECHO_AGENT_DEPLOYMENT_KEY).
    pub deployment_key: Option<String>,
    /// Optional extra auth material forwarded with requests when present.
    pub alpha_test_key: Option<String>,
    /// Release channel: "stable" or "alpha". Loaded from config.
    pub channel: String,
    /// Custom npm registry URL. When set, passed as `--registry=` to npm CLI.
    pub npm_registry: Option<String>,
}

impl UpdateConfig {
    pub fn from_environment(env: &EchoAgentBuildEnvironment) -> Self {
        Self {
            proxy_base_url: env.cli_chat_proxy_base_url(),
            auth_scope: echo_agent_runtime::auth::EchoAgentComConfig::default().auth_scope(),
            deployment_key: None,
            alpha_test_key: None,
            channel: "stable".to_string(),
            npm_registry: None,
        }
    }
}

#[derive(Debug, serde::Serialize, Deserialize)]
struct EchoAgentVersion {
    version: String,
    #[serde(default)]
    stable_version: Option<String>,
    checked_at: String,
}

impl EchoAgentVersion {
    fn is_fresh(&self, now: time::OffsetDateTime, ttl: Duration) -> bool {
        if let Ok(dt) = time::OffsetDateTime::parse(
            &self.checked_at,
            &time::format_description::well_known::Rfc3339,
        ) {
            // Clock-skew guard: future timestamps are never fresh.
            if dt > now {
                return false;
            }
            now - dt < ttl
        } else {
            false
        }
    }

    fn new(version: String, stable_version: Option<String>, now: time::OffsetDateTime) -> Self {
        let checked_at = now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| now.to_string());
        Self {
            version,
            stable_version,
            checked_at,
        }
    }
}

/// Return the semver-greater of two version strings.
fn semver_max(a: &str, b: &str) -> Result<String> {
    let va = semver::Version::parse(a)?;
    let vb = semver::Version::parse(b)?;
    Ok(std::cmp::max(va, vb).to_string())
}

/// Fetch the latest version from npm registry using `npm view`.
/// For alpha channel, fetches both `@alpha` and `@latest` dist-tags and
/// returns the semver-greater — prevents alpha users getting stuck when a
/// newer stable ships without updating the alpha dist-tag.
async fn fetch_npm_version(channel: &str, npm_registry: Option<&str>) -> Result<String> {
    if channel == "alpha" {
        let (alpha_v, stable_v) = tokio::try_join!(
            fetch_npm_tag("alpha", npm_registry),
            fetch_npm_tag("latest", npm_registry),
        )?;
        return semver_max(&alpha_v, &stable_v);
    }
    fetch_npm_tag("latest", npm_registry).await
}

/// Test-only entry point: invokes the private [`fetch_npm_tag`] for tests
/// that swap in a fake `npm` via PATH.
#[doc(hidden)]
pub async fn fetch_npm_tag_for_test(tag: &str, npm_registry: Option<&str>) -> Result<String> {
    fetch_npm_tag(tag, npm_registry).await
}

/// Test-only entry point: invokes the private [`fetch_npm_version`] for tests
/// that swap in a fake `npm` via PATH.
#[doc(hidden)]
pub async fn fetch_npm_version_for_test(
    channel: &str,
    npm_registry: Option<&str>,
) -> Result<String> {
    fetch_npm_version(channel, npm_registry).await
}

async fn fetch_npm_tag(tag: &str, npm_registry: Option<&str>) -> Result<String> {
    let pkg_spec = if tag == "latest" {
        NPM_PACKAGE.to_string()
    } else {
        format!("{}@{}", NPM_PACKAGE, tag)
    };
    let mut args = vec!["view", &pkg_spec, "version", "--json"];
    let registry_flag;
    if let Some(registry) = npm_registry {
        registry_flag = format!("--registry={}", registry);
        args.push(&registry_flag);
    }
    let mut cmd = Command::new("npm");
    cmd.args(&args).stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    let output = cmd.output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("npm view @{} failed: {}", tag, stderr.trim());
    }

    let stdout = String::from_utf8(output.stdout)?;
    let value: Value = serde_json::from_str(stdout.trim())?;
    match value {
        Value::String(version) => Ok(version),
        Value::Array(values) => values
            .iter()
            .rev()
            .find_map(|entry| entry.as_str().map(|item| item.to_string()))
            .ok_or_else(|| anyhow::anyhow!("npm view @{} returned empty version list", tag)),
        _ => anyhow::bail!("npm view @{} returned unexpected JSON", tag),
    }
}

/// Fetch the latest version from GitHub Releases using `gh release list`.
/// For alpha channel, fetches both pre-release and stable-only, returns the
/// semver-greater — `gh release list --limit 1` orders by publication date,
/// not semver, so we need both to guarantee correctness.
#[doc(hidden)]
pub async fn fetch_gh_release_version(channel: &str) -> Result<String> {
    if channel == "alpha" {
        let (with_pre, stable_only) = tokio::try_join!(
            fetch_gh_release_latest(false),
            fetch_gh_release_latest(true),
        )?;
        return semver_max(&with_pre, &stable_only);
    }
    fetch_gh_release_latest(true).await
}

async fn fetch_gh_release_latest(exclude_pre: bool) -> Result<String> {
    let mut args = vec![
        "release",
        "list",
        "--repo",
        GH_RELEASE_REPO,
        "--limit",
        "1",
        "--exclude-drafts",
        "--json",
        "tagName",
        "--jq",
        ".[0].tagName",
    ];
    if exclude_pre {
        args.push("--exclude-pre-releases");
    }
    let mut cmd = Command::new("gh");
    cmd.args(&args).stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    let output = cmd.output().await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh release list failed: {}", stderr.trim());
    }

    let tag = String::from_utf8(output.stdout)?.trim().to_string();
    // Tags are formatted as "v0.1.141", strip the leading "v"
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_string();
    if version.is_empty() {
        anyhow::bail!("No releases found in {}", GH_RELEASE_REPO);
    }
    Ok(version)
}

/// Fetch the latest version from a public CLI channel pointer.
///
/// Reads `{base}/{channel}` which contains a plain-text semver string
/// (e.g. `0.1.181`). No auth required — the upstream bucket is public.
///
/// For the alpha channel, fetches both `alpha` and `stable` pointers and
/// returns the semver-greater, matching the behavior of the npm and
/// gh-release paths.
///
/// Tries each base URL in [`CLI_BASE_URLS`] in order and stops at the first
/// success. Each individual base also retries up to 3 times with exponential
/// backoff (1s, 2s, 4s) on transient failures before falling through to the
/// next base.
pub(crate) async fn fetch_gcs_version(channel: &str) -> Result<String> {
    let mut last_err: Option<anyhow::Error> = None;
    let bases = cli_base_urls();
    for (i, base) in bases.iter().enumerate() {
        match fetch_gcs_version_from_base(channel, base).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if i + 1 < bases.len() {
                    tracing::warn!(
                        "channel pointer fetch from {} failed ({:#}); trying next base URL",
                        base,
                        e
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no CLI base URLs configured")))
}

/// Test-only entry point: same as [`fetch_gcs_version`] but reads from
/// `base_url` instead of the hardcoded GCS bucket.
#[doc(hidden)]
pub async fn fetch_gcs_version_from_base(channel: &str, base_url: &str) -> Result<String> {
    if channel == "alpha" {
        let (alpha_v, stable_v) = tokio::try_join!(
            fetch_gcs_channel_pointer("alpha", base_url),
            fetch_gcs_channel_pointer("stable", base_url),
        )?;
        return semver_max(&alpha_v, &stable_v);
    }
    fetch_gcs_channel_pointer(channel, base_url).await
}

async fn fetch_gcs_channel_pointer(channel: &str, base_url: &str) -> Result<String> {
    let url = format!("{}/{}", base_url, channel);
    let client = echo_agent_extra_ca::build_reqwest_client(|builder| {
        builder.timeout(Duration::from_secs(15))
    })?;

    let max_retries: u32 = 3;
    let mut last_err = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
        }
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(anyhow::anyhow!(
                    "GCS channel pointer fetch failed for {}: {:#}",
                    url,
                    e
                ));
                continue;
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            last_err = Some(anyhow::anyhow!(
                "GCS channel pointer fetch failed: HTTP {} for {}: {}",
                status,
                url,
                body.chars().take(200).collect::<String>().trim()
            ));
            continue;
        }
        match resp.text().await {
            Ok(body) => {
                let version = body.trim().to_string();
                if version.is_empty() {
                    last_err = Some(anyhow::anyhow!(
                        "empty {} channel pointer at {}",
                        channel,
                        url
                    ));
                    continue;
                }
                if semver::Version::parse(&version).is_err() {
                    anyhow::bail!(
                        "invalid semver in {} channel pointer: '{}'",
                        channel,
                        version
                    );
                }
                return Ok(version);
            }
            Err(e) => {
                last_err = Some(anyhow::anyhow!(
                    "GCS channel pointer body read failed for {}: {:#}",
                    url,
                    e
                ));
                continue;
            }
        }
    }
    Err(last_err.unwrap())
}

/// Fetch the latest version for the given installer type without writing the
/// version cache. Use this when the caller needs to control when the cache is
/// written (e.g. auto-update should only cache after a successful install or
/// when no update is needed).
pub async fn fetch_latest_version(installer: &str, config: &UpdateConfig) -> Result<String> {
    match installer {
        "npm" => fetch_npm_version(&config.channel, config.npm_registry.as_deref()).await,
        "gh-release" => fetch_gh_release_version(&config.channel).await,
        _ => fetch_gcs_version(&config.channel).await,
    }
}

/// Write the version cache to disk, recording that `version` was seen at the
/// current time. Call after confirming the version is current (no update
/// needed) or after a successful install.
///
/// `stable_version` records the current stable channel pointer so that
/// `channel_label()` can derive `[alpha]` vs `[stable]` without network I/O.
pub async fn write_version_cache(version: &str, stable_version: Option<&str>) {
    let version_path = echo_agent_home().join("version.json");
    let now = time::OffsetDateTime::now_utc();
    let json = EchoAgentVersion::new(
        version.to_string(),
        stable_version.map(|s| s.to_string()),
        now,
    );
    if let Some(dir) = version_path.parent()
        && let Err(e) = fs::create_dir_all(dir).await
    {
        tracing::warn!("failed to create version cache directory: {}", e);
        return;
    }
    let tmp = version_path.with_extension("json.tmp");
    let data = match serde_json::to_vec_pretty(&json) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to serialize version cache: {}", e);
            return;
        }
    };
    if let Err(e) = fs::write(&tmp, data).await {
        tracing::warn!("failed to write version cache tmp file: {}", e);
        return;
    }
    if let Err(e) = fs::rename(&tmp, &version_path).await {
        tracing::warn!("failed to rename version cache file: {}", e);
    }
}

/// Fetch the latest version for the given installer type and cache it.
///
/// Each installer is fully independent — no cross-installer fallback.
///
/// - `"npm"` — uses `npm view` against the public registry.
/// - `"internal"` — reads the channel pointer from the public GCS bucket.
/// - `"gh-release"` — uses `gh release list` against GitHub Releases.
pub async fn get_latest_version(installer: &str, config: &UpdateConfig) -> Result<String> {
    let version = fetch_latest_version(installer, config).await?;
    let stable_ptr = try_fetch_stable_pointer().await;
    write_version_cache(&version, stable_ptr.as_deref()).await;
    Ok(version)
}

/// True if `version.json` exists and is within TTL.
pub async fn is_version_cache_fresh() -> bool {
    let version_path = echo_agent_home().join("version.json");
    let now = time::OffsetDateTime::now_utc();
    if let Ok(version_str) = fs::read_to_string(&version_path).await
        && let Ok(version) = serde_json::from_str::<EchoAgentVersion>(&version_str)
        && version.is_fresh(now, TTL_SECONDS_BEFORE_AUTO_UPDATE)
    {
        return true;
    }
    false
}

pub use echo_agent_version::installed as get_installed_echo_agent_version;

/// Version of the managed echoagent binary currently on disk, read from the
/// `~/.echo-agent/bin/echoagent` symlink target (`../downloads/echo-agent-<version>-<platform>`)
/// without exec'ing anything.
///
/// Concurrent updaters (TUI background download, leader hourly checker,
/// explicit `echoagent update`) decide staleness from this instead of their own
/// compiled-in version, so a binary another process already installed is
/// never downloaded a second time.
///
/// Returns `None` when there is no parseable managed symlink (Windows
/// copy-based installs, dev builds) or when the symlink is DANGLING — a
/// link whose target binary was deleted (e.g. manual `~/.echo-agent/downloads`
/// cleanup) must not report an installed version, or every updater would
/// claim "already up to date" forever while no runnable binary exists.
/// NOTE: the symlink existing does not prove the *active installer*
/// maintains it — npm manages its own global install and a leftover symlink
/// from a previous internal install would lie about the npm install's
/// version. Callers must gate on the installer (see
/// `disk_version_for_installer` in `auto_update`).
pub fn installed_on_disk_version() -> Option<String> {
    #[cfg(unix)]
    {
        let app = echo_agent_runtime::util::echo_agent_home::echo_agent_application();
        let target = std::fs::read_link(&app).ok()?;
        // metadata() follows the symlink: Err means the target is gone
        // (dangling link) and the version it names is not actually on disk.
        std::fs::metadata(&app).ok()?;
        version_from_versioned_binary_name(target.file_name()?.to_str()?, "echo-agent")
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Extract the `<version>` portion of a versioned binary file name.
///
/// Handles the internal layout (`echo-agent-0.1.150-macos-aarch64`, including
/// pre-releases: `echo-agent-0.1.150-alpha.1-linux-x86_64` → `0.1.150-alpha.1`)
/// and the npm layout without a platform suffix (`echo-agent-0.1.150`,
/// `echo-agent-0.1.150-alpha.1`): everything between the `{bin_prefix}-` prefix
/// and the first platform-OS component is the version, validated as semver
/// so unknown layouts (`echo-agent-latest`, `echo-agent-pager-*` when `bin_prefix` is
/// `echo-agent`) return `None` instead of garbage.
///
/// Shared by the disk-version probe above and `cleanup_old_downloads` in
/// `auto_update` — keep it the single place that understands this naming.
pub(crate) fn version_from_versioned_binary_name(name: &str, bin_prefix: &str) -> Option<String> {
    const PLATFORM_OS: &[&str] = &["macos", "linux", "darwin", "windows"];
    let suffix = name.strip_prefix(bin_prefix)?.strip_prefix('-')?;
    let parts: Vec<&str> = suffix.split('-').collect();
    let platform_start = parts
        .iter()
        .position(|p| PLATFORM_OS.contains(p))
        .unwrap_or(parts.len());
    let ver_str = parts[..platform_start].join("-");
    semver::Version::parse(&ver_str).ok()?;
    Some(ver_str)
}

/// Fetch the stable channel pointer for caching alongside the version.
///
/// Tries each base URL in [`CLI_BASE_URLS`] and returns the first success.
/// Best-effort: returns `None` on any failure (the caller will simply omit
/// the stable pointer from the cache, and `channel_label()` will return `""`
/// until the next successful fetch).
///
/// The entire operation is capped at 500 ms. The stable pointer is only used
/// to derive the `[alpha]`/`[stable]` channel label — it is never required
/// for correctness. On slow or unreachable networks the timeout fires and we
/// return `None`; the label will populate on the next successful TTL check
/// (~30 min). This keeps startup and post-install paths fast.
pub(crate) async fn try_fetch_stable_pointer() -> Option<String> {
    tokio::time::timeout(Duration::from_millis(500), async {
        for base in cli_base_urls() {
            if let Ok(v) = fetch_gcs_channel_pointer("stable", &base).await {
                return Some(v);
            }
        }
        None
    })
    .await
    .unwrap_or(None)
}

/// Read the cached stable version from `~/.echo-agent/version.json` (sync, for display).
///
/// Returns `None` if the file doesn't exist, can't be parsed, or has no
/// `stable_version` field (e.g. written by an older binary).
pub fn cached_stable_version() -> Option<String> {
    let version_path = echo_agent_home().join("version.json");
    let content = std::fs::read_to_string(&version_path).ok()?;
    let gv: EchoAgentVersion = serde_json::from_str(&content).ok()?;
    gv.stable_version
}

/// Pure comparison: derive the channel name from current vs stable pointer.
///
/// Returns `Some("alpha")` when `current > stable`, `Some("stable")` when
/// `current <= stable`, or `None` when either version fails to parse.
fn derive_channel<'a>(current: &str, stable: &str) -> Option<&'a str> {
    let current_v = semver::Version::parse(current).ok()?;
    let stable_v = semver::Version::parse(stable).ok()?;
    if current_v > stable_v {
        Some("alpha")
    } else {
        Some("stable")
    }
}

/// Machine-readable channel name derived from the cached stable pointer.
///
/// Returns `Some("alpha")` when the current version is ahead of the cached
/// stable pointer, `Some("stable")` when at or behind, or `None` when no
/// cached pointer is available (first launch, old cache format, parse error).
///
/// The result is computed once and cached for the process lifetime.
pub fn channel_name() -> Option<&'static str> {
    use std::sync::OnceLock;
    static NAME: OnceLock<Option<&'static str>> = OnceLock::new();
    *NAME.get_or_init(|| {
        let stable = cached_stable_version()?;
        derive_channel(echo_agent_version::VERSION, &stable)
    })
}

/// Channel label derived from the cached stable pointer.
///
/// Compares the compiled-in `VERSION` against the stable pointer stored in
/// `~/.echo-agent/version.json` (written by the auto-updater):
/// - `" [alpha]"` when the current version is ahead of stable,
/// - `" [stable]"` when at or behind stable,
/// - `""` when no cached pointer is available (first launch, old cache format).
///
/// The result is computed once and cached for the process lifetime.
pub fn channel_label() -> &'static str {
    use std::sync::OnceLock;
    static LABEL: OnceLock<&'static str> = OnceLock::new();
    LABEL.get_or_init(|| {
        let stable = match cached_stable_version() {
            Some(s) => s,
            None => return "",
        };
        match derive_channel(echo_agent_version::VERSION, &stable) {
            Some("alpha") => " [alpha]",
            Some(_) => " [stable]",
            None => "",
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn loopback_base_rejects_userinfo_and_non_loopback() {
        use super::is_loopback_base;
        assert!(is_loopback_base("http://127.0.0.1:8971"));
        assert!(is_loopback_base("http://localhost:8971"));
        assert!(is_loopback_base("http://[::1]:8971"));
        // Prefix-check bypass vectors.
        assert!(!is_loopback_base("http://127.0.0.1:9@evil.com"));
        assert!(!is_loopback_base("http://localhost.evil.com:80"));
        assert!(!is_loopback_base("https://echo-agent.invalid/cli"));
        assert!(!is_loopback_base("http://192.168.1.1:80"));
        assert!(!is_loopback_base(""));
    }

    use super::*;

    /// Verifies that a future `checked_at` timestamp (e.g. from clock skew or
    /// NTP time-warp) is never considered fresh. Without the clock-skew guard
    /// this would return true indefinitely, silently disabling auto-update.
    #[test]
    fn test_is_fresh_rejects_future_timestamp() {
        let now = time::OffsetDateTime::now_utc();
        let future = now + Duration::from_secs(600);
        let v = EchoAgentVersion::new("0.1.200".to_string(), None, future);
        assert!(
            !v.is_fresh(now, Duration::from_secs(30)),
            "Future timestamp must not be considered fresh (clock-skew guard)."
        );
    }

    /// Disk-version probe: parsing the version out of the managed install's
    /// symlink-target file name (`echo-agent-<version>-<platform>`).
    #[test]
    fn test_version_from_versioned_binary_name() {
        let cases: &[(&str, Option<&str>)] = &[
            ("echo-agent-0.2.46-darwin-arm64", Some("0.2.46")),
            ("echo-agent-0.1.220-linux-x86_64", Some("0.1.220")),
            ("echo-agent-0.2.5-windows-x86_64.exe", Some("0.2.5")),
            // Pre-releases must round-trip whole — truncating to "0.1.220"
            // would make an alpha install masquerade as the release and
            // mask alpha → stable updates.
            (
                "echo-agent-0.1.220-alpha.4-linux-x86_64",
                Some("0.1.220-alpha.4"),
            ),
            ("echo-agent-0.1.220-alpha.4", Some("0.1.220-alpha.4")), // npm layout
            ("echo-agent-pager-0.1.5-darwin-arm64", None),           // "pager" is not a version
            ("echo-agent-garbage-darwin-arm64", None),               // unparseable version
            ("echo-agent-0.2.46", Some("0.2.46")),                   // no platform suffix
            ("other-0.2.46-darwin-arm64", None),                     // wrong prefix
            ("echo-agent-latest", None),                             // symlink alias, not a version
            ("echoagent", None),                                     // bare name
            ("", None),
        ];
        for (name, expected) in cases {
            assert_eq!(
                version_from_versioned_binary_name(name, "echo-agent").as_deref(),
                *expected,
                "version_from_versioned_binary_name({name:?})"
            );
        }

        // bin_prefix discrimination: the pager binary parses under its own
        // prefix but not under "echo-agent".
        assert_eq!(
            version_from_versioned_binary_name(
                "echo-agent-pager-0.1.5-darwin-arm64",
                "echo-agent-pager"
            )
            .as_deref(),
            Some("0.1.5")
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // derive_channel — invariant matrix
    //
    // Tests the pure comparison logic that determines [alpha] vs [stable].
    // Covers current 0.1.X-alpha.N, future 0.2.X, edge cases, and errors.
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn test_derive_channel_matrix() {
        // (current, stable_pointer, expected_channel)
        let cases: &[(&str, &str, Option<&str>)] = &[
            // ── Current 0.1.X workflow ──
            ("0.1.220-alpha.2", "0.1.219", Some("alpha")), // alpha ahead of stable
            ("0.1.219", "0.1.219", Some("stable")),        // stable user on latest
            ("0.1.218", "0.1.219", Some("stable")),        // stable user behind latest
            ("0.1.220-alpha.2", "0.1.220-alpha.2", Some("stable")), // pointer matches exactly
            ("0.1.220-alpha.2", "0.1.220", Some("stable")), // semver: release > pre-release
            // ── Future 0.2.X workflow ──
            ("0.2.5", "0.2.3", Some("alpha")), // alpha ahead of stable
            ("0.2.5", "0.2.5", Some("stable")), // promoted to stable
            ("0.2.3", "0.2.5", Some("stable")), // behind stable
            ("0.2.0", "0.2.0", Some("stable")), // first release, both 0.2.0
            // ── Cross-regime upgrade ──
            ("0.2.0", "0.1.219", Some("alpha")), // new regime ahead of old stable
            ("0.1.220-alpha.2", "0.2.0", Some("stable")), // old pre-release < new stable
            // ── Error cases ──
            ("garbage", "0.1.219", None), // unparseable current
            ("0.1.219", "garbage", None), // unparseable stable
            ("", "0.1.219", None),        // empty current
            ("0.1.219", "", None),        // empty stable
        ];

        for (current, stable, expected) in cases {
            let result = derive_channel(current, stable);
            assert_eq!(
                result, *expected,
                "derive_channel({:?}, {:?}) = {:?}, expected {:?}",
                current, stable, result, expected,
            );
        }
    }

    // ──────────────────────────────────────────────────────────────────────
    // semver_max — invariant matrix
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn test_semver_max_matrix() {
        // (a, b, expected)
        let cases: &[(&str, &str, &str)] = &[
            ("0.1.140", "0.1.140", "0.1.140"),                         // equal
            ("0.1.140", "0.1.141", "0.1.141"),                         // b higher
            ("0.1.141", "0.1.140", "0.1.141"),                         // a higher
            ("0.1.148-alpha.3", "0.1.148", "0.1.148"),                 // release > pre-release
            ("0.1.148", "0.1.148-alpha.3", "0.1.148"),                 // commutative
            ("0.1.148-alpha.1", "0.1.148-alpha.3", "0.1.148-alpha.3"), // pre-release ordering
            ("0.1.149-alpha.1", "0.1.148", "0.1.149-alpha.1"),         // higher base wins
            ("0.0.0", "0.0.1", "0.0.1"),                               // zero versions
            ("0.99.99", "1.0.0", "1.0.0"),                             // major jump
        ];

        for (a, b, expected) in cases {
            assert_eq!(
                semver_max(a, b).unwrap(),
                *expected,
                "semver_max({:?}, {:?})",
                a,
                b,
            );
        }
    }

    #[test]
    fn test_semver_max_invalid_input_returns_err() {
        assert!(semver_max("garbage", "0.1.141").is_err());
        assert!(semver_max("0.1.141", "garbage").is_err());
        assert!(semver_max("foo", "bar").is_err());
    }

    // ──────────────────────────────────────────────────────────────────────
    // EchoAgentVersion JSON shape — backward compatibility invariants
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn test_version_json_backward_compat() {
        // Old format (no stable_version) must parse — serde(default) fills None.
        let old = r#"{"version":"0.1.180","checked_at":"2026-04-22T10:30:00Z"}"#;
        let v: EchoAgentVersion = serde_json::from_str(old).unwrap();
        assert_eq!(v.version, "0.1.180");
        assert!(v.stable_version.is_none());

        // New format with stable_version round-trips correctly.
        let now = time::OffsetDateTime::now_utc();
        let new = EchoAgentVersion::new("0.2.5".to_string(), Some("0.2.3".to_string()), now);
        let json = serde_json::to_string(&new).unwrap();
        let parsed: EchoAgentVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, "0.2.5");
        assert_eq!(parsed.stable_version.as_deref(), Some("0.2.3"));

        // checked_at must be valid RFC3339.
        assert!(
            time::OffsetDateTime::parse(
                &parsed.checked_at,
                &time::format_description::well_known::Rfc3339,
            )
            .is_ok()
        );

        // Unknown fields are ignored (forward-compat).
        let future = r#"{"version":"0.1.180","checked_at":"2026-04-22T10:30:00Z","future":"ok"}"#;
        assert!(serde_json::from_str::<EchoAgentVersion>(future).is_ok());

        // Missing required field (checked_at) is rejected.
        let missing = r#"{"version":"0.1.180"}"#;
        assert!(serde_json::from_str::<EchoAgentVersion>(missing).is_err());
    }

    // ──────────────────────────────────────────────────────────────────────
    // is_fresh — TTL boundary invariants
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn test_is_fresh_ttl_boundaries() {
        let now = time::OffsetDateTime::now_utc();
        let v = EchoAgentVersion::new("0.1.200".to_string(), None, now);

        // Within TTL → fresh
        assert!(v.is_fresh(now, Duration::from_secs(60)));
        assert!(v.is_fresh(now + Duration::from_secs(29), Duration::from_secs(30)));

        // At TTL boundary → NOT fresh (strict <)
        assert!(!v.is_fresh(now + Duration::from_secs(30), Duration::from_secs(30)));

        // Past TTL → not fresh
        assert!(!v.is_fresh(now + Duration::from_secs(31), Duration::from_secs(30)));

        // Zero TTL → never fresh
        assert!(!v.is_fresh(now, Duration::ZERO));

        // Malformed timestamp → not fresh
        let bad = EchoAgentVersion {
            version: "0.1.200".to_string(),
            stable_version: None,
            checked_at: "not-rfc3339".to_string(),
        };
        assert!(!bad.is_fresh(now, Duration::from_secs(60)));
    }

    // ──────────────────────────────────────────────────────────────────────
    // UpdateConfig defaults
    // ──────────────────────────────────────────────────────────────────────

    #[test]
    fn test_update_config_default_channel_is_stable() {
        use echo_agent_runtime::env::EchoAgentBuildEnvironment;
        let cfg = UpdateConfig::from_environment(&EchoAgentBuildEnvironment::Production);
        assert_eq!(cfg.channel, "stable");
    }
}
