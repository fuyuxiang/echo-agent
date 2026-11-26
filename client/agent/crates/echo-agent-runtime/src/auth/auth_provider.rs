//! Model auth providers (`[auth_provider.<name>]`).
//!
//! A model opts in with `auth_provider = "<name>"`; the named table declares a
//! command that prints a fresh bearer token, which this module mints, caches,
//! and rotates for that model's requests.
//!
//! The minted token stays in memory only ([`AUTH_PROVIDER_SLOTS`] and chat
//! state, never `auth.json`); the command is a credential helper that owns its
//! own durable storage and OAuth2 refresh. See "Where model auth providers fit
//! (and don't)"
//! in `docs/internal/AUTH.md`.
//!
//! This is distinct from the `AuthCredentialProvider` HTTP consumers in
//! [`crate::auth::credential_provider`].

use super::token_output::{expiry_after_seconds, parse_token_output};

/// One named `[auth_provider.<name>]` table, honored only from the trusted
/// config layers (`parse_auth_providers`). A new field here needs a
/// `parse_auth_providers` warning decision.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct AuthProviderConfig {
    /// Command to run; without `args` it uses the platform shell, with `args` it execs directly.
    pub command: String,
    /// Command arguments; when set (even empty) the command execs directly.
    pub args: Option<Vec<String>>,
    /// Fallback token lifetime used when the output carries no `expires_in`.
    pub token_ttl_secs: Option<u64>,
    /// Max seconds to wait for the command (default 30, clamped to 1..=600).
    pub timeout_secs: Option<u64>,
    /// Working directory for the command; a leading `~` expands to home.
    pub cwd: Option<String>,
}

impl AuthProviderConfig {
    pub(crate) fn is_usable(&self) -> bool {
        !self.command.trim().is_empty()
    }
}

/// A model's reference to a named auth provider, built by `resolve_model_list`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(from = "AuthProviderRefData", into = "AuthProviderRefData")]
pub struct AuthProviderRef {
    pub(crate) name: String,
    pub(crate) config: AuthProviderConfig,
    slot: ProviderSlot,
    /// `true` once the trusted table is attached. A ref revived from bytes is
    /// `false` and never mints or reads until [`AuthProviderRef::attach_trusted_config`]
    /// joins the shared slot for its name.
    resolved: bool,
    fail_closed: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AuthProviderRefData {
    name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    fail_closed: bool,
}

impl From<AuthProviderRefData> for AuthProviderRef {
    fn from(data: AuthProviderRefData) -> Self {
        if data.fail_closed {
            AuthProviderRef::fail_closed(data.name)
        } else {
            AuthProviderRef::unresolved(data.name)
        }
    }
}

impl From<AuthProviderRef> for AuthProviderRefData {
    fn from(provider: AuthProviderRef) -> Self {
        Self {
            name: provider.name,
            fail_closed: provider.fail_closed,
        }
    }
}

impl AuthProviderRef {
    /// Production uses `unresolved` + `attach_trusted_config`.
    #[cfg(test)]
    pub(crate) fn new(name: String, config: AuthProviderConfig) -> Self {
        let slot = provider_slot(&name);
        Self {
            name,
            config,
            slot,
            resolved: true,
            fail_closed: false,
        }
    }

    /// The in-memory form of a ref revived from bytes;
    /// [`AuthProviderRef::attach_trusted_config`] resolves it.
    pub(crate) fn unresolved(name: String) -> Self {
        Self {
            name,
            config: AuthProviderConfig::default(),
            slot: ProviderSlot::default(),
            resolved: false,
            fail_closed: false,
        }
    }

    pub(crate) fn fail_closed(name: String) -> Self {
        Self {
            name,
            config: AuthProviderConfig::default(),
            slot: ProviderSlot::default(),
            resolved: true,
            fail_closed: true,
        }
    }

    pub(crate) fn is_fail_closed(&self) -> bool {
        self.fail_closed
    }

    /// Re-attach the trusted config for this name at model resolution
    /// (`None` = the table was removed, leaving an unusable config). The ref
    /// becomes authoritative, joins the shared slot for its name, and may mint.
    pub(crate) fn attach_trusted_config(&mut self, config: Option<&AuthProviderConfig>) {
        if self.fail_closed {
            return;
        }
        self.config = config.cloned().unwrap_or_default();
        self.slot = provider_slot(&self.name);
        self.resolved = true;
    }
}

/// Ignores the slot; a deserialized ref compares unequal until resolution
/// re-attaches its config.
impl PartialEq for AuthProviderRef {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.config == other.config
    }
}

impl Eq for AuthProviderRef {}

impl std::fmt::Debug for AuthProviderRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthProviderRef")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("resolved", &self.resolved)
            .finish_non_exhaustive()
    }
}

struct MintedProviderToken {
    token: String,
    /// Handed back to the command on the next run; never sent on the wire.
    refresh_token: Option<String>,
    /// Drives the 401 fresh-mint guard.
    minted_at: std::time::Instant,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The table version that minted the token; a different version reads as
    /// stale (see [`token_identity`]), so edits re-mint.
    minted_with: AuthProviderConfig,
}

/// The async lock is held across the command run, single-flighting mints
/// per provider name (shared across sessions). This dedupes concurrent
/// successes; a persistently failing helper is retried per waiter, each bounded
/// by the timeout clamp.
type ProviderSlot = std::sync::Arc<tokio::sync::Mutex<Option<MintedProviderToken>>>;

/// Shared token slots, one per resolved provider name. Bounded by the configured
/// provider names (only `attach_trusted_config` and test `new` insert), so no
/// eviction.
static AUTH_PROVIDER_SLOTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, ProviderSlot>>,
> = std::sync::OnceLock::new();

fn provider_slot(name: &str) -> ProviderSlot {
    let map = AUTH_PROVIDER_SLOTS.get_or_init(Default::default);
    let mut map = map.lock().unwrap_or_else(|e| e.into_inner());
    map.entry(name.to_owned()).or_default().clone()
}

/// Pre-refresh margin: re-mint when the token expires within this window.
pub(crate) const PROVIDER_TOKEN_EXPIRY_SKEW_SECS: u64 = 60;
const PROVIDER_TOKEN_EXPIRY_SKEW: chrono::Duration =
    chrono::Duration::seconds(PROVIDER_TOKEN_EXPIRY_SKEW_SECS as i64);
/// 401 fresh-mint guard: a token minted this recently is never re-minted on
/// rejection. Same idea as the guard in `unauthorized_recovery`, with a shorter
/// window because a provider mint is local and cheap.
const PROVIDER_TOKEN_FRESH_MINT_GUARD: std::time::Duration = std::time::Duration::from_secs(30);
const DEFAULT_PROVIDER_TIMEOUT_SECS: u64 = 30;
/// The effective mint timeout is clamped to `[1, this]`. A configured value
/// outside the range is honored up to the bound and draws a parse warning,
/// since a turn waits on the mint.
pub(crate) const PROVIDER_TIMEOUT_CEILING_SECS: u64 = 600;
/// Caps on the helper's captured output so a runaway command can't exhaust
/// memory before the timeout fires. A bearer (even a large JWT) is far under
/// the stdout cap; stderr only ever appears truncated in the failure log.
const PROVIDER_STDOUT_CAP_BYTES: u64 = 1 << 20; // 1 MiB
const PROVIDER_STDERR_CAP_BYTES: u64 = 64 << 10; // 64 KiB

/// The table fields that shape the minted token; a cached token minted under a
/// different set reads as stale, so a config edit re-mints. Destructured so a
/// new `AuthProviderConfig` field is a compile error until it is classified as
/// token-shaping (add it here) or an execution knob like `timeout_secs`
/// (editing it never invalidates).
fn token_identity(
    config: &AuthProviderConfig,
) -> (&str, Option<&[String]>, Option<u64>, Option<&str>) {
    let AuthProviderConfig {
        command,
        args,
        token_ttl_secs,
        timeout_secs: _,
        cwd,
    } = config;
    (command, args.as_deref(), *token_ttl_secs, cwd.as_deref())
}

fn minted_token_is_stale(minted: &MintedProviderToken, config: &AuthProviderConfig) -> bool {
    token_identity(&minted.minted_with) != token_identity(config)
        || minted
            .expires_at
            .is_some_and(|at| chrono::Utc::now() + PROVIDER_TOKEN_EXPIRY_SKEW >= at)
}

/// Log the missing-command warning once per provider, then at debug, so a
/// misconfigured model doesn't warn on every turn.
fn warn_empty_command(name: &str) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let first = WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name.to_owned());
    const MSG: &str = "auth provider has no usable command: the [auth_provider.*] table is \
                       missing from the trusted config layers, or its `command` is empty";
    if first {
        tracing::warn!(provider = %name, "{MSG}");
    } else {
        tracing::debug!(provider = %name, "{MSG}");
    }
}

/// Read up to `keep` bytes into `buf`, then drain and discard any remainder so
/// the child never blocks on a full pipe. Memory stays bounded by `keep`.
async fn read_capped<R>(reader: R, keep: u64, buf: &mut Vec<u8>) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut limited = reader.take(keep);
    limited.read_to_end(buf).await?;
    tokio::io::copy(&mut limited.into_inner(), &mut tokio::io::sink()).await?;
    Ok(())
}

/// Remove every first-party credential from the helper's environment. BYOK
/// isolates these keys on the wire, so the helper (the agent puts them in its
/// own env at startup) must not inherit them.
fn scrub_first_party_credentials(cmd: &mut tokio::process::Command) {
    for var in crate::agent::config::FIRST_PARTY_CREDENTIAL_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Spawn `cmd`, capture stdout/stderr with a byte cap (reading both
/// concurrently so a full pipe on one can't deadlock the other; a runaway helper
/// is drained to a sink past the cap so it can't wedge the wait), and bound the
/// whole run by `timeout`. Exceeding the stdout cap is an error.
///
/// On timeout the child's entire process group is killed. The helper is a group
/// leader (`detach_command`'s `setsid`), so a compound `sh -c` helper's
/// grandchildren -- and the `ECHO_AGENT_AUTH_PROVIDER_*` credentials in their env --
/// do not outlive the reported timeout; `kill_on_drop` alone would reap only the
/// direct child.
async fn run_capped(
    cmd: &mut tokio::process::Command,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    #[allow(clippy::disallowed_methods)] // killed at the timeout this call reports
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("command failed to start: {e}"))?;
    // Enroll the child's process group so the timeout path can tear down the
    // whole tree. Best-effort: if enrollment fails, `kill_on_drop` still reaps
    // the direct child.
    let mut group = echo_agent_tools::util::ProcessGroup::new()
        .map_err(|e| anyhow::anyhow!("process group setup failed: {e}"))?;
    if let Err(e) = group.attach(&child) {
        tracing::debug!(error = %e, "auth provider: could not enroll helper process group");
    }
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();

    // One extra stdout byte so an over-cap write is detectable, not truncated.
    // The stderr read is advisory (it only feeds the failure log), so only
    // stdout governs the mint.
    let capture = async {
        let (out_res, err_res) = tokio::join!(
            read_capped(stdout, PROVIDER_STDOUT_CAP_BYTES + 1, &mut out_buf),
            read_capped(stderr, PROVIDER_STDERR_CAP_BYTES, &mut err_buf),
        );
        if let Err(e) = err_res {
            tracing::debug!(error = %e, "auth provider: stderr capture failed (advisory)");
        }
        out_res.map_err(|e| anyhow::anyhow!("reading command stdout: {e}"))?;
        child
            .wait()
            .await
            .map_err(|e| anyhow::anyhow!("waiting on command: {e}"))
    };

    let status = match tokio::time::timeout(timeout, capture).await {
        Ok(res) => res?,
        Err(_elapsed) => {
            let _ = group.kill();
            anyhow::bail!("command timed out after {}s", timeout.as_secs());
        }
    };
    if out_buf.len() as u64 > PROVIDER_STDOUT_CAP_BYTES {
        anyhow::bail!("command wrote more than {PROVIDER_STDOUT_CAP_BYTES} bytes to stdout");
    }
    Ok(std::process::Output {
        status,
        stdout: out_buf,
        stderr: err_buf,
    })
}

fn resolve_program(command: &str, cwd: Option<&std::path::Path>) -> std::path::PathBuf {
    let path = std::path::Path::new(command);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if path.components().count() > 1
        && let Some(dir) = cwd
    {
        return dir.join(path);
    }
    std::path::PathBuf::from(command)
}

async fn mint_provider_token(
    provider: &AuthProviderRef,
    mark_expired: bool,
    previous: Option<&MintedProviderToken>,
) -> anyhow::Result<MintedProviderToken> {
    use std::process::Stdio;

    let name = &provider.name;
    let config = &provider.config;
    // Clamp to [1, ceiling]: the slot lock is held across the run, so an
    // unbounded timeout would let one hung helper stall every turn sharing this
    // provider name. The ceiling is a hard bound, not just a parse warning.
    let timeout_secs = config
        .timeout_secs
        .unwrap_or(DEFAULT_PROVIDER_TIMEOUT_SECS)
        .clamp(1, PROVIDER_TIMEOUT_CEILING_SECS);
    tracing::info!(
        provider = %name,
        mark_expired,
        timeout_secs,
        "auth provider: running helper command"
    );

    let cwd = config
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(crate::util::expand_home);

    let mut cmd = match config.args {
        Some(ref args) => {
            let program = resolve_program(config.command.trim(), cwd.as_deref());
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args);
            cmd
        }
        None => crate::util::subprocess::shell_c(config.command.as_str()),
    };
    if let Some(ref dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Capture stderr for the failure log; inheriting corrupts the TUI.
        .stderr(Stdio::piped())
        // Reaps the direct child if the future is dropped; `run_capped`
        // additionally kills the whole process group on timeout.
        .kill_on_drop(true);
    if mark_expired {
        cmd.env("ECHO_AGENT_AUTH_EXPIRED", "1");
    }
    // Git-credential-helper handback: give the command the last stored
    // credential so it can refresh instead of re-authenticating.
    if let Some(prev) = previous {
        cmd.env("ECHO_AGENT_AUTH_PROVIDER_ACCESS_TOKEN", &prev.token);
        if let Some(refresh) = &prev.refresh_token {
            cmd.env("ECHO_AGENT_AUTH_PROVIDER_REFRESH_TOKEN", refresh);
        }
        if let Some(expires_at) = prev.expires_at {
            cmd.env(
                "ECHO_AGENT_AUTH_PROVIDER_EXPIRES_AT",
                expires_at.to_rfc3339(),
            );
        }
    }
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    // Scrub last so nothing above can reintroduce a first-party credential.
    scrub_first_party_credentials(&mut cmd);

    let output = run_capped(&mut cmd, std::time::Duration::from_secs(timeout_secs)).await?;

    let parsed = match parse_token_output(&output) {
        Ok(parsed) => parsed,
        Err(e) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "{e} (stderr: {})",
                crate::util::truncate(stderr.trim(), 300)
            );
        }
    };
    let expires_at = parsed
        .expires_at
        .or_else(|| config.token_ttl_secs.and_then(expiry_after_seconds))
        .or_else(|| crate::auth::parse_jwt_expiration(&parsed.access_token));
    tracing::info!(
        provider = %name,
        mark_expired,
        expires_at = ?expires_at,
        "auth provider minted token"
    );
    Ok(MintedProviderToken {
        token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        minted_at: std::time::Instant::now(),
        expires_at,
        minted_with: config.clone(),
    })
}

#[derive(Debug, PartialEq, Eq)]
#[must_use = "a rotated token must be written to chat-state, or the wire keeps the stale key"]
pub(crate) enum ProviderRefreshOutcome {
    /// `current_key` is already the fresh cached token; nothing to write.
    Unchanged,
    /// A token that should replace `current_key` on the wire.
    Rotated(String),
    /// The provider is unusable (unresolved or removed); already warned.
    Unusable,
    /// The mint ran and failed (logged).
    MintFailed,
}

impl ProviderRefreshOutcome {
    pub(crate) fn rotated(self) -> Option<String> {
        match self {
            Self::Rotated(token) => Some(token),
            Self::Unchanged | Self::Unusable | Self::MintFailed => None,
        }
    }
}

impl AuthProviderRef {
    /// The slot, locked for a mutating operation. A removed provider drops
    /// its cached token and yields `None`, failing closed. An unresolved ref
    /// (revived from bytes) fails closed without touching the shared slot.
    async fn locked_slot(
        &self,
    ) -> Option<tokio::sync::OwnedMutexGuard<Option<MintedProviderToken>>> {
        if !self.resolved {
            return None;
        }
        let mut slot = self.slot.clone().lock_owned().await;
        if !self.config.is_usable() {
            if slot.take().is_some() {
                tracing::warn!(
                    provider = %self.name,
                    "auth provider removed from config: dropping its cached token"
                );
            }
            if !self.fail_closed {
                warn_empty_command(&self.name);
            }
            return None;
        }
        Some(slot)
    }

    /// Cache-only read for sync resolution: never runs the command, blocks, or
    /// mutates. `None` for an unresolved ref, a cold or stale cache, or a mint
    /// in progress; minting happens pre-turn via [`AuthProviderRef::ensure_fresh_token`].
    pub(crate) fn cached_token(&self) -> Option<String> {
        if !self.resolved {
            return None;
        }
        if !self.config.is_usable() {
            if !self.fail_closed {
                warn_empty_command(&self.name);
            }
            return None;
        }
        // A mint in progress holds the lock; treat it as a miss rather than
        // block the sync path.
        let Ok(guard) = self.slot.try_lock() else {
            tracing::debug!(provider = %self.name, "cache read skipped: mint in progress");
            return None;
        };
        guard
            .as_ref()
            .filter(|m| !minted_token_is_stale(m, &self.config))
            .map(|m| m.token.clone())
    }

    /// The token that should replace `current_key` on the wire: serves the
    /// fresh cached token when chat-state lags behind a rotation, mints when
    /// the cache is cold or stale. Mints or rotates a bearer; unrelated to an
    /// OAuth refresh token.
    pub(crate) async fn ensure_fresh_token(
        &self,
        current_key: Option<&str>,
    ) -> ProviderRefreshOutcome {
        let Some(mut slot) = self.locked_slot().await else {
            return ProviderRefreshOutcome::Unusable;
        };
        if let Some(ref minted) = *slot
            && !minted_token_is_stale(minted, &self.config)
        {
            return if current_key == Some(minted.token.as_str()) {
                ProviderRefreshOutcome::Unchanged
            } else {
                ProviderRefreshOutcome::Rotated(minted.token.clone())
            };
        }
        let mark_expired = slot.is_some();
        let minted = match mint_provider_token(self, mark_expired, slot.as_ref()).await {
            Ok(minted) => minted,
            Err(e) => {
                tracing::warn!(
                    provider = %self.name,
                    error = %e,
                    "auth provider pre-turn mint failed"
                );
                return ProviderRefreshOutcome::MintFailed;
            }
        };
        let token = minted.token.clone();
        *slot = Some(minted);
        ProviderRefreshOutcome::Rotated(token)
    }

    /// The replacement for a server-rejected `rejected_key` (chat-state's
    /// current key): a fresher cached token is adopted without a re-run,
    /// otherwise the command runs once. `None` for a token minted moments ago
    /// under the current table (the fresh-mint guard, which an edited table
    /// bypasses).
    pub(crate) async fn recover_rejected_token(&self, rejected_key: &str) -> Option<String> {
        let mut slot = self.locked_slot().await?;
        if let Some(ref minted) = *slot {
            if minted.token != rejected_key && !minted_token_is_stale(minted, &self.config) {
                return Some(minted.token.clone());
            }
            if minted.token == rejected_key
                && token_identity(&minted.minted_with) == token_identity(&self.config)
                && minted.minted_at.elapsed() < PROVIDER_TOKEN_FRESH_MINT_GUARD
            {
                tracing::warn!(
                    provider = %self.name,
                    "auth provider token rejected moments after mint: not \
                     re-running (fresh-mint guard); surfacing the 401"
                );
                return None;
            }
        }
        tracing::info!(provider = %self.name, "auth provider token rejected: re-minting");
        let minted = match mint_provider_token(self, true, slot.as_ref()).await {
            Ok(minted) => minted,
            Err(e) => {
                tracing::warn!(
                    provider = %self.name,
                    error = %e,
                    "auth provider 401 re-mint failed"
                );
                // The server rejected the cached token and the re-mint failed;
                // mark it stale so it is not re-served next turn (fail closed).
                // The entry stays so its refresh token still feeds the next
                // handback attempt.
                if let Some(minted) = slot.as_mut() {
                    minted.expires_at = Some(chrono::Utc::now());
                }
                return None;
            }
        };
        let token = minted.token.clone();
        *slot = Some(minted);
        Some(token)
    }
}

/// Backdate a provider's mint time past the fresh-mint guard.
#[cfg(test)]
pub(crate) fn test_backdate_provider_mint(name: &str, age: std::time::Duration) {
    let slot = provider_slot(name);
    let mut slot = slot
        .try_lock()
        .expect("no mint in flight during test mutation");
    if let Some(ref mut minted) = *slot {
        minted.minted_at = std::time::Instant::now()
            .checked_sub(age)
            .expect("backdate before the process epoch");
    }
}

/// A counting provider that prints "tok-1", "tok-2", ... on successive runs.
#[cfg(test)]
pub(crate) fn test_counting_provider(name: &str, dir: &std::path::Path) -> AuthProviderRef {
    let counter = dir.join("count");
    AuthProviderRef::new(
        name.to_owned(),
        AuthProviderConfig {
            command: format!(
                "echo run >> {c}; printf 'tok-%s' \"$(wc -l < {c} | tr -d ' ')\"",
                c = counter.display()
            ),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    )
}

#[cfg(test)]
fn test_expire_provider_token(name: &str) {
    let slot = provider_slot(name);
    let mut slot = slot
        .try_lock()
        .expect("no mint in flight during test mutation");
    if let Some(ref mut minted) = *slot {
        minted.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
    }
}

#[cfg(test)]
#[path = "auth_provider_tests.rs"]
mod tests;
