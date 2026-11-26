//! Resolves and caches the author identity for feedback submissions
//! ([`crate::agent::config::FeedbackUserConfig`]).

use crate::agent::config::FeedbackUserConfig;
use crate::util::subprocess::CommandLog;
use crate::util::subprocess::RunOptions;
use crate::util::subprocess::git_bin;
use crate::util::subprocess::run_detached_with_timeout;
use crate::util::subprocess::shell_c;
use std::env;
use std::time::Duration;
use std::time::Instant;
use strum::IntoEnumIterator;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::spawn_blocking;
use tracing::warn;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

const MAX_VALUE_LEN: usize = 256;

/// Incomplete resolutions retry after this; complete ones never expire.
const INCOMPLETE_RESULT_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedUserIdentity {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl ResolvedUserIdentity {
    fn slot(&self, field: IdentityField) -> &Option<String> {
        match field {
            IdentityField::Name => &self.name,
            IdentityField::Email => &self.email,
        }
    }

    fn slot_mut(&mut self, field: IdentityField) -> &mut Option<String> {
        match field {
            IdentityField::Name => &mut self.name,
            IdentityField::Email => &mut self.email,
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.name.is_none() && self.email.is_none()
    }
}

#[derive(serde::Deserialize)]
struct IdentityCommandOutput {
    name: Option<String>,
    email: Option<String>,
}

/// Trims and bounds length ([`MAX_VALUE_LEN`]); the server handles
/// character-level sanitization.
fn normalize_value(s: impl AsRef<str>) -> Option<String> {
    let trimmed = s.as_ref().trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_VALUE_LEN {
        warn!(
            len = trimmed.len(),
            "feedback identity: resolved value exceeds the length limit; ignoring it"
        );
        return None;
    }
    Some(trimmed.to_owned())
}

fn env_value(var: &str) -> Option<String> {
    env::var(var).ok().and_then(normalize_value)
}

/// A literal source value (usually `$VAR` / `${VAR}` already expanded at load).
/// A residual `$` is skipped as a likely-unset variable. This over-rejects a
/// rare genuine `$` so an unset variable is never emitted.
fn literal_value(value: &str) -> Option<String> {
    let value = normalize_value(value)?;
    if value.contains('$') {
        warn!("feedback identity: ignoring a source value with an unresolved variable");
        return None;
    }
    Some(value)
}

fn os_user() -> Option<String> {
    #[cfg(unix)]
    {
        // SUDO_USER can appear in an ordinary shell; trust it only when the
        // effective user ID is root.
        if nix::unistd::geteuid().is_root()
            && let Some(user) = env_value("SUDO_USER")
        {
            return Some(user);
        }
        if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::geteuid())
            && let Some(name) = normalize_value(&user.name)
        {
            return Some(name);
        }
    }
    env_value("USER")
        .or_else(|| env_value("LOGNAME"))
        .or_else(|| env_value("USERNAME"))
}

/// Runs `os_user` on a blocking thread with a timeout: the user lookup can
/// block (a directory service) and runs under the cache lock, so a stall must
/// not hold up feedback submissions. On timeout we stop waiting; the thread
/// finishes in the background holding no lock, and the pool reclaims it.
async fn resolve_os_user() -> Option<String> {
    match tokio::time::timeout(COMMAND_TIMEOUT, spawn_blocking(os_user)).await {
        Ok(Ok(name)) => name,
        Ok(Err(join_error)) => {
            warn!(error = %join_error, "feedback identity: os_user lookup task failed");
            None
        }
        Err(_elapsed) => {
            warn!("feedback identity: os_user lookup timed out");
            None
        }
    }
}

async fn git_global_email() -> Option<String> {
    let mut cmd = Command::new(git_bin());
    // `--global` only: a repo-local `.git/config` in a cloned repo could
    // otherwise supply an attacker-controlled email.
    cmd.args(["config", "--global", "user.email"]);
    let output = run_detached_with_timeout(
        cmd,
        COMMAND_TIMEOUT,
        RunOptions {
            label: "git config --global user.email",
            command_log: CommandLog::Redacted,
        },
    )
    .await
    .ok()?;
    if !output.status.success() {
        return None;
    }
    normalize_value(String::from_utf8_lossy(&output.stdout))
}

/// `None` on any failure; callers fall back to the declarative sources.
async fn run_identity_command(command: &str) -> Option<ResolvedUserIdentity> {
    let cmd = shell_c(command);
    let output = run_detached_with_timeout(
        cmd,
        COMMAND_TIMEOUT,
        RunOptions {
            label: "feedback identity command",
            command_log: CommandLog::Redacted,
        },
    )
    .await
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<IdentityCommandOutput>(stdout.trim()) {
        Ok(parsed) => Some(ResolvedUserIdentity {
            name: parsed.name.and_then(normalize_value),
            // Require the same address shape as the declarative sources.
            email: parsed
                .email
                .and_then(normalize_value)
                .filter(|email| IdentityField::Email.accepts(email)),
        }),
        Err(e) => {
            warn!(error = %e, "feedback identity: command output is not the expected JSON");
            None
        }
    }
}

#[derive(Debug)]
enum SourceToken {
    OsUser,
    GitEmail,
    /// A literal value, usually produced by `$VAR` / `${VAR}` expansion at
    /// config load; used as-is, subject to per-field validation.
    Literal(String),
}

impl SourceToken {
    fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "" => None,
            "os_user" => Some(Self::OsUser),
            "git_email" => Some(Self::GitEmail),
            other => Some(Self::Literal(other.to_owned())),
        }
    }

    /// A log-safe label: keyword tokens by name, literals redacted so a
    /// configured literal value never reaches the logs.
    fn label(&self) -> &'static str {
        match self {
            Self::OsUser => "os_user",
            Self::GitEmail => "git_email",
            Self::Literal(_) => "<literal>",
        }
    }

    /// The one field this token may populate, or `None` if it works for any.
    fn restricted_field(&self) -> Option<IdentityField> {
        match self {
            Self::GitEmail => Some(IdentityField::Email),
            Self::OsUser | Self::Literal(_) => None,
        }
    }

    async fn resolve(&self) -> Option<String> {
        match self {
            Self::OsUser => resolve_os_user().await,
            Self::GitEmail => git_global_email().await,
            Self::Literal(value) => literal_value(value),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter)]
enum IdentityField {
    Name,
    Email,
}

impl IdentityField {
    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Email => "email",
        }
    }

    /// Whether a resolved value is valid for this field. Emails must contain
    /// `@`; names have no shape requirement.
    fn accepts(self, value: &str) -> bool {
        match self {
            Self::Name => true,
            Self::Email => value.contains('@'),
        }
    }

    fn sources(self, cfg: &FeedbackUserConfig) -> &[String] {
        match self {
            Self::Name => &cfg.name,
            Self::Email => &cfg.email,
        }
    }

    /// Whether the config declares a source that could still fill this field
    /// later. `email_domain` is excluded: a derived `<name>@<domain>` depends
    /// only on the resolved name, so it won't change on retry.
    fn can_produce(self, cfg: &FeedbackUserConfig) -> bool {
        !self.sources(cfg).is_empty() || cfg.command.is_some()
    }
}

/// The first resolved source wins; misplaced or invalid values are logged and skipped.
async fn resolve_sources(entries: &[String], field: IdentityField) -> Option<String> {
    for entry in entries {
        let Some(token) = SourceToken::parse(entry) else {
            continue;
        };
        if let Some(restricted_field) = token.restricted_field()
            && restricted_field != field
        {
            warn!(
                token = %token.label(),
                field = field.key(),
                "feedback identity: ignoring a source token that only applies to the `{}` list",
                restricted_field.key()
            );
            continue;
        }
        if let Some(value) = token.resolve().await {
            // Skip a value invalid for this field, such as a bare username in
            // the email list.
            if !field.accepts(&value) {
                warn!(
                    token = %token.label(),
                    field = field.key(),
                    "feedback identity: ignoring a value that is not valid for this field"
                );
                continue;
            }
            return Some(value);
        }
    }
    None
}

/// Builds `<name>@<domain>`, used only after every declared email source fails.
fn derive_email(name: &str, email_domain: Option<&str>) -> Option<String> {
    if name.contains('@') || name.contains(char::is_whitespace) {
        return None;
    }
    // Tolerate the admin typo `email_domain = "@example.com"`.
    let domain = email_domain?.trim();
    let domain = domain.strip_prefix('@').unwrap_or(domain);
    if domain.is_empty()
        || domain.contains('@')
        // Reject an unset `$VAR` left verbatim by config-load expansion (matches
        // `literal_value`), so a derived address never embeds an unresolved var.
        || domain.contains('$')
        || domain.contains(char::is_whitespace)
    {
        return None;
    }
    normalize_value(format!("{name}@{domain}"))
}

/// May run subprocesses; production callers go through [`cached_identity`].
async fn resolve_identity(cfg: &FeedbackUserConfig) -> ResolvedUserIdentity {
    // The command is admin-provided config: trimmed only, never length-checked,
    // so long or multi-line commands still run. Its output is bounded below.
    let mut identity = match cfg
        .command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        Some(command) => run_identity_command(command).await.unwrap_or_default(),
        None => ResolvedUserIdentity::default(),
    };

    for field in IdentityField::iter() {
        if identity.slot(field).is_none() {
            let resolved = resolve_sources(field.sources(cfg), field).await;
            *identity.slot_mut(field) = resolved;
        }
    }
    if identity.email.is_none()
        && let Some(name) = identity.name.as_deref()
    {
        identity.email = derive_email(name, cfg.email_domain.as_deref());
    }
    identity
}

struct CacheEntry {
    cfg: FeedbackUserConfig,
    identity: ResolvedUserIdentity,
    resolved_at: Instant,
}

impl CacheEntry {
    /// Whether every field this config can populate has a value. Complete
    /// entries never expire; incomplete ones are retried after
    /// [`INCOMPLETE_RESULT_TTL`] so a source that wasn't ready (git, a slow
    /// command) can fill in later.
    fn is_complete(&self) -> bool {
        IdentityField::iter()
            .all(|field| self.identity.slot(field).is_some() || !field.can_produce(&self.cfg))
    }
}

/// Single-slot cache keyed on the config; incomplete results expire after
/// [`INCOMPLETE_RESULT_TTL`].
pub(crate) struct IdentityCache {
    slot: Mutex<Option<CacheEntry>>,
}

impl IdentityCache {
    pub(crate) const fn new() -> Self {
        Self {
            slot: Mutex::const_new(None),
        }
    }

    /// Holds the lock across resolution so concurrent submissions run a slow
    /// command once, not once each. The tradeoff: submissions serialize, and a
    /// first-time resolution can take up to the combined source timeouts. The
    /// pre-warm at session spawn keeps this off the interactive path.
    pub(crate) async fn get(
        &self,
        cfg: Option<&FeedbackUserConfig>,
    ) -> Option<ResolvedUserIdentity> {
        self.get_at(cfg, Instant::now()).await
    }

    async fn get_at(
        &self,
        cfg: Option<&FeedbackUserConfig>,
        now: Instant,
    ) -> Option<ResolvedUserIdentity> {
        let cfg = cfg?;
        let mut slot = self.slot.lock().await;
        if let Some(entry) = slot.as_ref()
            && entry.cfg == *cfg
            && (entry.is_complete()
                || now.saturating_duration_since(entry.resolved_at) < INCOMPLETE_RESULT_TTL)
        {
            return Some(entry.identity.clone());
        }
        let mut identity = resolve_identity(cfg).await;
        // A same-config re-resolution is a retry to fill missing fields, not to
        // drop resolved ones: keep any field a prior attempt resolved when this
        // attempt's source transiently fails (an `os_user` stall, a flaky git
        // or command).
        if let Some(prev) = slot.as_ref().filter(|entry| entry.cfg == *cfg) {
            for field in IdentityField::iter() {
                if identity.slot(field).is_none()
                    && let Some(value) = prev.identity.slot(field)
                {
                    *identity.slot_mut(field) = Some(value.clone());
                }
            }
        }
        *slot = Some(CacheEntry {
            cfg: cfg.clone(),
            identity: identity.clone(),
            resolved_at: now,
        });
        Some(identity)
    }
}

impl Default for IdentityCache {
    fn default() -> Self {
        Self::new()
    }
}

static PROCESS_CACHE: IdentityCache = IdentityCache::new();

pub(crate) async fn cached_identity(
    cfg: Option<&FeedbackUserConfig>,
) -> Option<ResolvedUserIdentity> {
    PROCESS_CACHE.get(cfg).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_test_support::env::EnvGuard;

    fn cfg() -> FeedbackUserConfig {
        FeedbackUserConfig::default()
    }

    /// Runs the async resolver on a throwaway current-thread runtime so unit
    /// tests can stay synchronous.
    fn resolve(cfg: FeedbackUserConfig) -> ResolvedUserIdentity {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(resolve_identity(&cfg))
    }

    #[test]
    #[serial_test::serial]
    fn os_user_resolves_to_current_user() {
        let _no_sudo = EnvGuard::unset("SUDO_USER");
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["os_user".into()],
            ..cfg()
        });
        let name = resolved.name.expect("os_user must resolve on CI/dev");
        assert!(!name.trim().is_empty());
        assert!(resolved.email.is_none());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn os_user_ignores_sudo_user_when_not_root() {
        if nix::unistd::geteuid().is_root() {
            // Under root the SUDO_USER branch is the correct one.
            return;
        }
        let _sudo = EnvGuard::set("SUDO_USER", "realuser");
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["os_user".into()],
            ..cfg()
        });
        let name = resolved.name.expect("os_user must still resolve");
        assert_ne!(
            name, "realuser",
            "a lingering SUDO_USER must not be trusted without a root euid"
        );
    }

    #[test]
    fn first_resolvable_literal_wins() {
        // Empty, unexpanded-`$`, and oversized entries are skipped; the first
        // usable, trimmed value wins.
        let resolved = resolve(FeedbackUserConfig {
            name: vec![
                "".into(),
                "$UNSET".into(),
                "x".repeat(300),
                "  alice  ".into(),
            ],
            ..cfg()
        });
        assert_eq!(resolved.name.as_deref(), Some("alice"));

        // A list with no usable entry resolves to nothing.
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["x".repeat(300)],
            ..cfg()
        });
        assert_eq!(resolved.name, None);
    }

    #[test]
    #[serial_test::serial]
    fn git_email_resolves_from_global_git_config() {
        let dir = tempfile::tempdir().unwrap();
        let gitconfig = dir.path().join("gitconfig");
        std::fs::write(&gitconfig, "[user]\n\temail = admin@example.com\n").unwrap();
        let _global = EnvGuard::set("GIT_CONFIG_GLOBAL", &gitconfig);
        let resolved = resolve(FeedbackUserConfig {
            email: vec!["git_email".into()],
            ..cfg()
        });
        assert_eq!(resolved.email.as_deref(), Some("admin@example.com"));
        assert_eq!(resolved.name, None);
    }

    #[test]
    #[serial_test::serial]
    fn git_email_is_invalid_in_name_list() {
        // Point git at a resolvable config so the rejection can't pass vacuously.
        let dir = tempfile::tempdir().unwrap();
        let gitconfig = dir.path().join("gitconfig");
        std::fs::write(&gitconfig, "[user]\n\temail = admin@example.com\n").unwrap();
        let _global = EnvGuard::set("GIT_CONFIG_GLOBAL", &gitconfig);
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["git_email".into()],
            ..cfg()
        });
        assert_eq!(resolved.name, None);
    }

    #[test]
    fn non_address_value_is_rejected_from_the_email_list() {
        let resolved = resolve(FeedbackUserConfig {
            email: vec!["alice".into()],
            ..cfg()
        });
        assert_eq!(resolved.email, None);
    }

    #[test]
    fn email_domain_derives_only_when_email_sources_fail() {
        // No usable email source: derive from name@domain.
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["carol".into()],
            email: vec!["$UNSET_EMAIL".into()],
            email_domain: Some("example.com".into()),
            ..cfg()
        });
        assert_eq!(resolved.email.as_deref(), Some("carol@example.com"));

        // A resolved email source wins; domain derivation must not override it.
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["carol".into()],
            email: vec!["carol@corp.example".into()],
            email_domain: Some("example.com".into()),
            ..cfg()
        });
        assert_eq!(resolved.email.as_deref(), Some("carol@corp.example"));
    }

    #[test]
    fn email_domain_derivation_guards() {
        // No username at all: nothing to derive from.
        let resolved = resolve(FeedbackUserConfig {
            email_domain: Some("example.com".into()),
            ..cfg()
        });
        assert_eq!(resolved.email, None);
        assert!(resolved.is_empty());

        // Leading `@` in the domain (admin typo) is tolerated.
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["carol".into()],
            email_domain: Some("@example.com".into()),
            ..cfg()
        });
        assert_eq!(resolved.email.as_deref(), Some("carol@example.com"));

        // A username containing `@` or whitespace derives nothing.
        for bad_user in ["carol@corp", "carol jones"] {
            let resolved = resolve(FeedbackUserConfig {
                name: vec![bad_user.into()],
                email_domain: Some("example.com".into()),
                ..cfg()
            });
            assert_eq!(resolved.email, None, "name {bad_user:?}");
        }

        // A domain with whitespace, a non-leading `@`, or an unresolved `$VAR`
        // derives nothing.
        for bad_domain in ["example com", "corp@example.com", "@@example.com", "$CORP"] {
            let resolved = resolve(FeedbackUserConfig {
                name: vec!["carol".into()],
                email_domain: Some(bad_domain.into()),
                ..cfg()
            });
            assert_eq!(resolved.email, None, "email_domain {bad_domain:?}");
        }
    }

    /// Multi-line and >[`MAX_VALUE_LEN`]-byte commands must reach the shell,
    /// not be rejected for exceeding the length limit.
    #[test]
    fn multi_line_and_long_commands_still_run() {
        let multi_line = "true\necho '{\"name\": \"multi-line\"}'";
        let resolved = resolve(FeedbackUserConfig {
            command: Some(multi_line.into()),
            ..cfg()
        });
        assert_eq!(resolved.name.as_deref(), Some("multi-line"));

        let long = format!("{}echo '{{\"name\": \"long-cmd\"}}'", "true && ".repeat(40));
        assert!(long.len() > MAX_VALUE_LEN);
        let resolved = resolve(FeedbackUserConfig {
            command: Some(long),
            ..cfg()
        });
        assert_eq!(resolved.name.as_deref(), Some("long-cmd"));
    }

    #[test]
    fn command_output_wins_then_falls_back_per_field() {
        // A command's emitted fields win over the declarative sources.
        let resolved = resolve(FeedbackUserConfig {
            name: vec!["os_user".into()],
            command: Some(r#"echo '{"name": "from-cmd", "email": "cmd@example.com"}'"#.into()),
            ..cfg()
        });
        assert_eq!(resolved.name.as_deref(), Some("from-cmd"));
        assert_eq!(resolved.email.as_deref(), Some("cmd@example.com"));

        // A field the command omits falls back to the declarative source.
        let resolved = resolve(FeedbackUserConfig {
            email: vec!["fallback@example.com".into()],
            command: Some(r#"echo '{"name": "cmd-user"}'"#.into()),
            ..cfg()
        });
        assert_eq!(resolved.name.as_deref(), Some("cmd-user"));
        assert_eq!(resolved.email.as_deref(), Some("fallback@example.com"));

        // A command-emitted email without `@` is rejected like a literal one, so
        // resolution falls back to the declarative email source.
        let resolved = resolve(FeedbackUserConfig {
            email: vec!["fallback@example.com".into()],
            command: Some(r#"echo '{"name": "cmd-user", "email": "not-an-email"}'"#.into()),
            ..cfg()
        });
        assert_eq!(resolved.email.as_deref(), Some("fallback@example.com"));

        // Whitespace-only command fields count as unresolved too.
        let resolved = resolve(FeedbackUserConfig {
            command: Some(r#"echo '{"name": "  ", "email": ""}'"#.into()),
            ..cfg()
        });
        assert!(resolved.is_empty());
    }

    #[test]
    fn command_failure_falls_back_to_declarative_sources() {
        for command in [
            "/nonexistent/echo-agent-identity-binary", // command not found: sh exits 127
            "exit 3",                                  // nonzero exit
            "echo not-json",                           // unparseable stdout
        ] {
            let resolved = resolve(FeedbackUserConfig {
                name: vec!["decl-user".into()],
                command: Some(command.into()),
                ..cfg()
            });
            assert_eq!(
                resolved.name.as_deref(),
                Some("decl-user"),
                "command {command:?} must fall back to declarative sources"
            );
        }
    }

    #[tokio::test]
    async fn cache_resolves_once_per_config_and_reresolves_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs");
        let command = format!(
            "echo run >> {}; echo '{{\"name\": \"cached-user\"}}'",
            counter.display()
        );
        let cache = IdentityCache::new();

        assert_eq!(cache.get(None).await, None, "no opt-in, no identity");

        let cfg1 = FeedbackUserConfig {
            command: Some(command.clone()),
            ..cfg()
        };
        let first = cache.get(Some(&cfg1)).await.unwrap();
        assert_eq!(first.name.as_deref(), Some("cached-user"));
        let second = cache.get(Some(&cfg1)).await.unwrap();
        assert_eq!(second, first);
        let runs = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(runs.lines().count(), 1, "same config must resolve once");

        let cfg2 = FeedbackUserConfig {
            command: Some(command),
            email_domain: Some("example.com".into()),
            ..cfg()
        };
        let third = cache.get(Some(&cfg2)).await.unwrap();
        assert_eq!(third.email.as_deref(), Some("cached-user@example.com"));
        let runs = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(runs.lines().count(), 2, "changed config must re-resolve");
    }

    /// Command emitting `first_json` on its first run and `then_json` after,
    /// appending one line per run to the returned counter file.
    fn first_then_command(
        dir: &std::path::Path,
        first_json: &str,
        then_json: &str,
    ) -> (String, std::path::PathBuf) {
        let counter = dir.join("runs");
        let marker = dir.join("ready");
        let command = format!(
            "echo run >> {c}; if [ -f {p} ]; then echo '{then_json}'; else touch {p}; echo '{first_json}'; fi",
            c = counter.display(),
            p = marker.display()
        );
        (command, counter)
    }

    fn run_count(counter: &std::path::Path) -> usize {
        std::fs::read_to_string(counter).unwrap().lines().count()
    }

    #[tokio::test]
    async fn cache_incomplete_results_expire_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        // First run resolves only the username; later runs resolve both.
        let (command, counter) = first_then_command(
            dir.path(),
            r#"{"name": "u1"}"#,
            r#"{"name": "u1", "email": "u1@example.com"}"#,
        );
        let cache = IdentityCache::new();
        let cfg1 = FeedbackUserConfig {
            command: Some(command),
            ..cfg()
        };

        let t0 = Instant::now();
        let first = cache.get_at(Some(&cfg1), t0).await.unwrap();
        assert_eq!(first.name.as_deref(), Some("u1"));
        assert_eq!(first.email, None, "first run leaves email unresolved");

        let second = cache
            .get_at(Some(&cfg1), t0 + Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(second, first);
        assert_eq!(
            run_count(&counter),
            1,
            "incomplete result cached within TTL"
        );

        let third = cache
            .get_at(
                Some(&cfg1),
                t0 + INCOMPLETE_RESULT_TTL + Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(
            third.email.as_deref(),
            Some("u1@example.com"),
            "an incomplete resolution must expire, not pin the unresolved field"
        );
        assert_eq!(run_count(&counter), 2);

        let fourth = cache
            .get_at(
                Some(&cfg1),
                t0 + INCOMPLETE_RESULT_TTL * 3 + Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(fourth, third);
        assert_eq!(run_count(&counter), 2, "complete results do not expire");
    }

    #[tokio::test]
    async fn cache_retry_keeps_a_previously_resolved_field() {
        // First run resolves only the name; the post-TTL retry resolves only the
        // email. The retry must not drop the name the first run already resolved.
        let dir = tempfile::tempdir().unwrap();
        let (command, counter) = first_then_command(
            dir.path(),
            r#"{"name": "u1"}"#,
            r#"{"email": "u1@example.com"}"#,
        );
        let cache = IdentityCache::new();
        let cfg = FeedbackUserConfig {
            command: Some(command),
            ..cfg()
        };

        let t0 = Instant::now();
        let first = cache.get_at(Some(&cfg), t0).await.unwrap();
        assert_eq!(first.name.as_deref(), Some("u1"));
        assert_eq!(first.email, None);

        let second = cache
            .get_at(
                Some(&cfg),
                t0 + INCOMPLETE_RESULT_TTL + Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(
            second.name.as_deref(),
            Some("u1"),
            "a transient retry failure must not drop the already-resolved name"
        );
        assert_eq!(second.email.as_deref(), Some("u1@example.com"));
        assert_eq!(run_count(&counter), 2);
    }

    #[test]
    fn name_only_config_settles_but_pending_email_source_stays_incomplete() {
        // A name-only config can never fill `email`, so a name-resolved entry is
        // complete and won't be retried on TTL.
        let complete = CacheEntry {
            cfg: FeedbackUserConfig {
                name: vec!["carol".into()],
                ..cfg()
            },
            identity: ResolvedUserIdentity {
                name: Some("carol".into()),
                email: None,
            },
            resolved_at: Instant::now(),
        };
        assert!(complete.is_complete());

        // With an email source declared but unresolved, the entry stays
        // incomplete and will be retried after the TTL.
        let incomplete = CacheEntry {
            cfg: FeedbackUserConfig {
                name: vec!["carol".into()],
                email: vec!["git_email".into()],
                ..cfg()
            },
            identity: ResolvedUserIdentity {
                name: Some("carol".into()),
                email: None,
            },
            resolved_at: Instant::now(),
        };
        assert!(!incomplete.is_complete());
    }

    #[tokio::test]
    async fn cache_single_flights_concurrent_resolution() {
        // Two concurrent submissions must run the slow command once, not once
        // each: the cache holds its lock across resolution so the work isn't
        // duplicated.
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs");
        let command = format!(
            "echo run >> {}; sleep 0.2; echo '{{\"name\": \"once\", \"email\": \"once@example.com\"}}'",
            counter.display()
        );
        let cache = IdentityCache::new();
        let cfg = FeedbackUserConfig {
            command: Some(command),
            ..cfg()
        };

        let (first, second) = tokio::join!(cache.get(Some(&cfg)), cache.get(Some(&cfg)));
        let first = first.unwrap();
        assert_eq!(first.name.as_deref(), Some("once"));
        assert_eq!(first, second.unwrap());
        assert_eq!(
            run_count(&counter),
            1,
            "concurrent submissions resolve once"
        );
    }
}
