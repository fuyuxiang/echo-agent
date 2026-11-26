//! Proactive, jittered OIDC token refresh owned by the workspace server.
//!
//! [`ProactiveOidcAuthProvider::current`] is a lock-free snapshot read and
//! never performs I/O. The background task refreshes ahead of expiry when
//! remaining lifetime exceeds `safety_margin`. `min_refresh_interval` floors
//! the gap after a successful refresh so a short TTL cannot hot-loop the
//! IdP; it does not delay a cold-start refresh that is already due.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use echo_agent_computer_hub_sdk::{
    AuthCredential, AuthIdentity, AuthProvider, OnRefreshCallback, PrincipalKey, RefreshEvent,
};
use prometheus::{
    Histogram, IntCounterVec, exponential_buckets, register_histogram, register_int_counter_vec,
};
use rand::Rng;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::status_config::ProactiveRefreshConfig;

/// Cadence used when the token has no `expires_at` (opaque / no-expiry).
const DEFAULT_NO_EXPIRY_INTERVAL: Duration = Duration::from_secs(3600);

/// Failure-retry base; doubles each attempt up to [`RETRY_CAP`].
const RETRY_BASE: Duration = Duration::from_secs(1);
/// Failure-retry ceiling. Not applied to the success-path schedule.
const RETRY_CAP: Duration = Duration::from_secs(30);
/// Upper bound on `Retry-After` so a malicious header cannot park the loop.
/// A zero / past value is treated as absent (see [`parse_retry_after_value`])
/// and [`bound_retry_after`] still floors at [`RETRY_BASE`].
const RETRY_AFTER_CAP: Duration = Duration::from_secs(24 * 3600);

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN_TIMEOUT: Duration = Duration::from_secs(15);

const OUTCOME_OK: &str = "ok";
const OUTCOME_FAILED_RETRY: &str = "failed_retry";
const OUTCOME_FAILED_EXHAUSTED: &str = "failed_exhausted";
const OUTCOME_FAILED_TERMINAL: &str = "failed_terminal";
const OUTCOME_SKIPPED_DISABLED: &str = "skipped_disabled";

static REFRESH_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_oidc_proactive_refresh_total",
        "Background OIDC refresh outcomes: ok, failed_retry, failed_exhausted, \
         failed_terminal, skipped_disabled",
        &["outcome"]
    )
    .expect("register echo_agent_workspace_oidc_proactive_refresh_total")
});

static REFRESH_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "echo_agent_workspace_oidc_proactive_refresh_duration_seconds",
        "Wall-clock time of a background OIDC refresh (discovery + token exchange)",
        exponential_buckets(0.01, 2.0, 14).expect("valid bucket params")
    )
    .expect("register echo_agent_workspace_oidc_proactive_refresh_duration_seconds")
});

static REFRESH_LEAD: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "echo_agent_workspace_oidc_refresh_lead_seconds",
        "Remaining lifetime of the token being replaced (old expires_at − now) \
         at a successful background refresh; negative means the refresh landed \
         after expiry",
        // Negative/zero bounds keep a post-expiry refresh out of the first
        // positive bucket (Prometheus treats the first bound as starting at 0).
        vec![
            -1440.0, -720.0, -360.0, -180.0, -60.0, -30.0, 0.0, 10.0, 30.0, 60.0, 120.0, 300.0,
            600.0, 900.0, 1200.0, 1800.0, 2400.0, 3600.0, 7200.0,
        ]
    )
    .expect("register echo_agent_workspace_oidc_refresh_lead_seconds")
});

static REFRESH_JITTER: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "echo_agent_workspace_oidc_refresh_jitter_seconds",
        "Signed displacement of the success-path target from its unclamped \
         nominal, after safety-margin and min-interval floors",
        vec![
            -1440.0, -720.0, -360.0, -180.0, -60.0, -30.0, 0.0, 30.0, 60.0, 180.0, 360.0, 720.0,
            1440.0,
        ]
    )
    .expect("register echo_agent_workspace_oidc_refresh_jitter_seconds")
});

/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    for outcome in [
        OUTCOME_OK,
        OUTCOME_FAILED_RETRY,
        OUTCOME_FAILED_EXHAUSTED,
        OUTCOME_FAILED_TERMINAL,
        OUTCOME_SKIPPED_DISABLED,
    ] {
        REFRESH_TOTAL.with_label_values(&[outcome]).inc_by(0);
    }
    let _ = &*REFRESH_DURATION;
    let _ = &*REFRESH_LEAD;
    let _ = &*REFRESH_JITTER;
}

struct TokenSnapshot {
    access_token: Arc<str>,
    expires_at: Option<DateTime<Utc>>,
    /// `None` at cold start (auth.json has no TTL); set from `expires_in`
    /// after the first successful refresh.
    observed_ttl: Option<Duration>,
}

/// Inputs for [`ProactiveOidcAuthProvider::new`].
pub struct ProactiveOidcParams {
    pub access_token: String,
    pub refresh_token: String,
    pub issuer: String,
    pub client_id: String,
    pub identity: AuthIdentity,
    pub expires_at: Option<DateTime<Utc>>,
    pub refresh: ProactiveRefreshConfig,
    pub on_refresh: Option<OnRefreshCallback>,
}

struct PersistGate {
    seq: AtomicU64,
    lock: parking_lot::Mutex<()>,
}

struct Inner {
    snapshot: ArcSwap<TokenSnapshot>,
    issuer: String,
    client_id: String,
    identity: AuthIdentity,
    cfg: ProactiveRefreshConfig,
    on_refresh: Option<OnRefreshCallback>,
    persist: Arc<PersistGate>,
}

struct RefreshOutcome {
    lead_secs: Option<f64>,
    new_refresh_token: Option<String>,
}

struct RefreshError {
    error: anyhow::Error,
    new_refresh_token: Option<String>,
    /// `400` / `invalid_grant` / `401` / `403`: stop the loop for the process life.
    terminal: bool,
    /// Honored on retryable errors (`429` `Retry-After`, etc.).
    retry_after: Option<Duration>,
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::fmt::Debug for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

fn refresh_err(error: impl Into<anyhow::Error>) -> RefreshError {
    RefreshError {
        error: error.into(),
        new_refresh_token: None,
        terminal: false,
        retry_after: None,
    }
}

/// Workspace-owned OIDC provider: lock-free `current()`, background refresh.
pub struct ProactiveOidcAuthProvider {
    inner: Arc<Inner>,
    cancel: CancellationToken,
    // JoinHandle detaches on drop; cancel + abort so a dropped provider
    // stops refreshing without leaving an IdP loop running.
    _task_guard: Option<AbortOnDropHandle<()>>,
}

impl Drop for ProactiveOidcAuthProvider {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl std::fmt::Debug for ProactiveOidcAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProactiveOidcAuthProvider")
            .field("issuer", &self.inner.issuer)
            .field("client_id", &self.inner.client_id)
            .field("identity", &self.inner.identity)
            .finish_non_exhaustive()
    }
}

impl ProactiveOidcAuthProvider {
    /// Spawn the background refresher. Must be called from a Tokio runtime.
    /// When `params.refresh.enabled` is false, no task is spawned and no
    /// IdP traffic is issued; `current()` still serves the seed snapshot.
    pub fn new(params: ProactiveOidcParams) -> Self {
        let mut refresh = params.refresh;
        refresh.validate();
        let cancel = CancellationToken::new();
        let enabled = refresh.enabled;
        let inner = Arc::new(Inner {
            snapshot: ArcSwap::from_pointee(TokenSnapshot {
                access_token: Arc::from(params.access_token),
                expires_at: params.expires_at,
                observed_ttl: None,
            }),
            issuer: params.issuer,
            client_id: params.client_id,
            identity: params.identity,
            cfg: refresh,
            on_refresh: params.on_refresh,
            persist: Arc::new(PersistGate {
                seq: AtomicU64::new(0),
                lock: parking_lot::Mutex::new(()),
            }),
        });
        if !enabled {
            REFRESH_TOTAL
                .with_label_values(&[OUTCOME_SKIPPED_DISABLED])
                .inc();
            return Self {
                inner,
                cancel,
                _task_guard: None,
            };
        }
        let task = tokio::spawn(refresh_loop(
            inner.clone(),
            params.refresh_token,
            cancel.clone(),
        ));
        Self {
            inner,
            cancel,
            _task_guard: Some(AbortOnDropHandle::new(task)),
        }
    }

    #[cfg(test)]
    fn swap_access_token(&self, token: &str) {
        let prev = self.inner.snapshot.load();
        self.inner.snapshot.store(Arc::new(TokenSnapshot {
            access_token: Arc::from(token),
            expires_at: prev.expires_at,
            observed_ttl: prev.observed_ttl,
        }));
    }

    #[cfg(test)]
    fn persist_for_test(
        &self,
        access_token: &str,
        new_refresh_token: Option<String>,
        expires_at: Option<DateTime<Utc>>,
        delay: Duration,
    ) {
        persist_refresh_event_after(
            &self.inner,
            access_token,
            new_refresh_token,
            expires_at,
            delay,
        );
    }
}

impl AuthProvider for ProactiveOidcAuthProvider {
    fn current(&self) -> AuthCredential {
        AuthCredential::bearer(self.inner.snapshot.load().access_token.as_ref())
    }

    fn principal_key(&self) -> PrincipalKey {
        // Match the SDK's `oidc:{issuer}:{client_id}:{user_id}` (empty
        // user_id still gets the trailing colon) so a drop-in swap cannot
        // fragment the connection pool.
        PrincipalKey::opaque(format!(
            "oidc:{}:{}:{}",
            self.inner.issuer, self.inner.client_id, self.inner.identity.user_id
        ))
    }

    fn identity(&self) -> Option<AuthIdentity> {
        Some(self.inner.identity.clone())
    }
}

async fn refresh_loop(inner: Arc<Inner>, mut refresh_token: String, cancel: CancellationToken) {
    let mut fail_attempt: u32 = 0;
    let mut exhausted = false;
    let mut pending: Option<(Duration, Option<f64>)> = None;
    loop {
        let (sleep_for, jitter_secs) = match pending.take() {
            Some(scheduled) => scheduled,
            None => next_sleep(&inner, fail_attempt),
        };
        if fail_attempt == 0
            && let Some(jitter) = jitter_secs
        {
            REFRESH_JITTER.observe(jitter);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            () = tokio::time::sleep(sleep_for) => {}
        }

        let started = Instant::now();
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            result = do_refresh(&inner, &refresh_token) => result,
        };
        match result {
            Ok(outcome) => {
                if let Some(rt) = outcome.new_refresh_token {
                    refresh_token = rt;
                }
                let duration_secs = started.elapsed().as_secs_f64();
                REFRESH_TOTAL.with_label_values(&[OUTCOME_OK]).inc();
                REFRESH_DURATION.observe(duration_secs);
                if let Some(lead) = outcome.lead_secs {
                    REFRESH_LEAD.observe(lead);
                }
                fail_attempt = 0;
                exhausted = false;
                let next = next_sleep(&inner, 0);
                tracing::info!(
                    lead_secs = outcome.lead_secs,
                    jitter_secs = next.1,
                    next_refresh_in_secs = next.0.as_secs_f64(),
                    outcome = OUTCOME_OK,
                    "OIDC token refreshed"
                );
                pending = Some(next);
            }
            Err(error) => {
                if let Some(rt) = error.new_refresh_token.clone() {
                    refresh_token = rt;
                }
                let duration_secs = started.elapsed().as_secs_f64();
                REFRESH_DURATION.observe(duration_secs);
                if error.terminal {
                    REFRESH_TOTAL
                        .with_label_values(&[OUTCOME_FAILED_TERMINAL])
                        .inc();
                    tracing::error!(
                        error = %error,
                        outcome = OUTCOME_FAILED_TERMINAL,
                        "OIDC refresh rejected; stopping proactive loop"
                    );
                    return;
                }
                fail_attempt = fail_attempt.saturating_add(1);
                if let Some(after) = error.retry_after {
                    pending = Some((bound_retry_after(after, &inner), None));
                }
                let now = Utc::now();
                let past_expiry = inner
                    .snapshot
                    .load()
                    .expires_at
                    .is_some_and(|exp| now >= exp);
                if past_expiry && !exhausted {
                    exhausted = true;
                    REFRESH_TOTAL
                        .with_label_values(&[OUTCOME_FAILED_EXHAUSTED])
                        .inc();
                    tracing::warn!(
                        error = %error,
                        outcome = OUTCOME_FAILED_EXHAUSTED,
                        "OIDC refresh exhausted the proactive window"
                    );
                } else {
                    REFRESH_TOTAL
                        .with_label_values(&[OUTCOME_FAILED_RETRY])
                        .inc();
                    tracing::warn!(
                        error = %error,
                        outcome = OUTCOME_FAILED_RETRY,
                        "OIDC refresh failed; retrying"
                    );
                }
            }
        }
    }
}

fn next_sleep(inner: &Inner, fail_attempt: u32) -> (Duration, Option<f64>) {
    let snap = inner.snapshot.load();
    let now = Utc::now();
    if fail_attempt == 0 {
        let unit = rand::rng().random_range(-1.0..=1.0);
        let (at, jitter) =
            compute_success_refresh_at(now, snap.expires_at, snap.observed_ttl, &inner.cfg, unit);
        let sleep = (at - now).to_std().unwrap_or(Duration::ZERO);
        (sleep, Some(jitter))
    } else {
        (
            compute_retry_delay(fail_attempt - 1, now, snap.expires_at),
            None,
        )
    }
}

/// Success-path target. `jitter_unit` is clamped to `[-1, 1]` and scales
/// the `±jitter_fraction · scale` window. Floors run after jitter; the
/// returned jitter is the post-constraint displacement from nominal.
pub(crate) fn compute_success_refresh_at(
    now: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    observed_ttl: Option<Duration>,
    cfg: &ProactiveRefreshConfig,
    jitter_unit: f64,
) -> (DateTime<Utc>, f64) {
    let jitter_unit = jitter_unit.clamp(-1.0, 1.0);
    let fallback = datetime_plus(now, cfg.min_refresh_interval).unwrap_or(now);
    let (nominal, raw_jitter) = match (expires_at, observed_ttl) {
        (Some(exp), Some(ttl)) => {
            let lead = ttl.mul_f64(1.0 - cfg.fraction);
            let nominal = datetime_minus(exp, lead).unwrap_or(fallback);
            let jitter_secs = ttl.as_secs_f64() * cfg.jitter_fraction * jitter_unit;
            (nominal, jitter_secs)
        }
        (Some(exp), None) => {
            let rem = remaining_std(exp, now);
            let nominal = datetime_plus(now, rem.mul_f64(cfg.fraction)).unwrap_or(fallback);
            let jitter_secs = rem.as_secs_f64() * cfg.jitter_fraction * jitter_unit;
            (nominal, jitter_secs)
        }
        (None, _) => (
            datetime_plus(now, DEFAULT_NO_EXPIRY_INTERVAL).unwrap_or(fallback),
            0.0,
        ),
    };

    let mut refresh_at = datetime_offset(nominal, raw_jitter).unwrap_or(nominal);

    if let Some(exp) = expires_at
        && let Some(cap) = datetime_minus(exp, cfg.safety_margin)
        && refresh_at > cap
    {
        refresh_at = cap;
    }
    // Floor only after a successful refresh. A cold-start seed that is
    // already expired or inside the safety window must refresh now —
    // `current()` never I/Os, so delaying would serve a stale bearer.
    if observed_ttl.is_some() {
        if let Some(floor) = datetime_plus(now, cfg.min_refresh_interval) {
            if refresh_at < floor {
                refresh_at = floor;
            }
        } else if refresh_at < now {
            refresh_at = now;
        }
    } else if refresh_at < now {
        refresh_at = now;
    }

    let effective_jitter = (refresh_at - nominal).num_milliseconds() as f64 / 1000.0;
    (refresh_at, effective_jitter)
}

/// Capped exponential backoff. Not floored by `min_refresh_interval` so a
/// last-ditch retry near expiry is not delayed. Before expiry the delay is
/// capped to remaining lifetime; at or after expiry [`RETRY_CAP`] avoids a spin.
pub(crate) fn compute_retry_delay(
    attempt: u32,
    now: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
) -> Duration {
    let factor = 2u32.saturating_pow(attempt);
    let backoff = RETRY_BASE.saturating_mul(factor).min(RETRY_CAP);
    match expires_at {
        Some(exp) if now < exp => backoff.min(remaining_std(exp, now)),
        Some(_) => RETRY_CAP,
        None => backoff,
    }
}

async fn do_refresh(inner: &Inner, refresh_token: &str) -> Result<RefreshOutcome, RefreshError> {
    let previous_expires_at = inner.snapshot.load().expires_at;
    let issuer = inner.issuer.trim_end_matches('/');
    let client =
        echo_agent_extra_ca::build_reqwest_client(|builder| builder).map_err(refresh_err)?;

    #[derive(serde::Deserialize)]
    struct Discovery {
        token_endpoint: String,
    }

    let disc: Discovery = require_success(
        client
            .get(format!("{issuer}/.well-known/openid-configuration"))
            .timeout(DISCOVERY_TIMEOUT)
            .send()
            .await
            .map_err(refresh_err)?,
    )
    .await?
    .json()
    .await
    .map_err(refresh_err)?;

    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", inner.client_id.as_str()),
    ];
    if let Some(ref v) = inner.identity.principal_type {
        params.push(("principal_type", v));
    }
    if let Some(ref v) = inner.identity.principal_id {
        params.push(("principal_id", v));
    }

    #[derive(serde::Deserialize)]
    struct Tokens {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<u64>,
    }

    let tokens: Tokens = require_success(
        client
            .post(&disc.token_endpoint)
            .form(&params)
            .timeout(TOKEN_TIMEOUT)
            .send()
            .await
            .map_err(refresh_err)?,
    )
    .await?
    .json()
    .await
    .map_err(refresh_err)?;

    let now = Utc::now();
    let (observed_ttl, expires_at) = match tokens.expires_in {
        Some(secs) => {
            let ttl = Duration::from_secs(secs);
            match datetime_plus(now, ttl) {
                Some(exp) => (Some(ttl), Some(exp)),
                None => {
                    // Keep the rotated RT in memory for the next retry, but
                    // do not persist this failed exchange — a late write
                    // would clobber a later successful persist.
                    return Err(RefreshError {
                        error: anyhow::anyhow!("OIDC expires_in out of range: {secs}"),
                        new_refresh_token: tokens.refresh_token,
                        terminal: false,
                        retry_after: None,
                    });
                }
            }
        }
        None => (None, None),
    };
    let lead_secs = previous_expires_at.map(|exp| (exp - now).num_milliseconds() as f64 / 1000.0);

    persist_refresh_event(
        inner,
        &tokens.access_token,
        tokens.refresh_token.clone(),
        expires_at,
    );

    inner.snapshot.store(Arc::new(TokenSnapshot {
        access_token: Arc::from(tokens.access_token),
        expires_at,
        observed_ttl,
    }));
    Ok(RefreshOutcome {
        lead_secs,
        new_refresh_token: tokens.refresh_token,
    })
}

fn persist_refresh_event(
    inner: &Inner,
    access_token: &str,
    new_refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
) {
    persist_refresh_event_after(
        inner,
        access_token,
        new_refresh_token,
        expires_at,
        Duration::ZERO,
    );
}

fn persist_refresh_event_after(
    inner: &Inner,
    access_token: &str,
    new_refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    delay: Duration,
) {
    let Some(cb) = inner.on_refresh.clone() else {
        return;
    };
    let event = RefreshEvent {
        access_token: access_token.to_owned(),
        new_refresh_token,
        expires_at,
    };
    let seq = inner.persist.seq.fetch_add(1, Ordering::AcqRel) + 1;
    let persist = inner.persist.clone();
    // Off the refresh task so flock cannot stall cancel/select!. The seq
    // + mutex drop a stale write that finishes after a newer persist.
    std::thread::spawn(move || {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let _guard = persist.lock.lock();
        if persist.seq.load(Ordering::Acquire) != seq {
            return;
        }
        cb(&event);
    });
}

fn is_terminal_auth_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST
            | reqwest::StatusCode::UNAUTHORIZED
            | reqwest::StatusCode::FORBIDDEN
    )
}

fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let raw = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    parse_retry_after_value(raw)
}

/// `Retry-After: 0` or a past HTTP-date is `None` so the loop keeps
/// exponential backoff instead of sleeping zero and hammering the IdP.
fn parse_retry_after_value(raw: &str) -> Option<Duration> {
    let after = if let Ok(secs) = raw.parse::<u64>() {
        Duration::from_secs(secs)
    } else {
        let dt = DateTime::parse_from_rfc2822(raw).ok()?;
        remaining_std(dt.with_timezone(&Utc), Utc::now())
    };
    (!after.is_zero()).then_some(after)
}

fn bound_retry_after(after: Duration, inner: &Inner) -> Duration {
    let after = after.max(RETRY_BASE).min(RETRY_AFTER_CAP);
    let snap = inner.snapshot.load();
    let bounded = match snap.expires_at {
        Some(exp) if Utc::now() < exp => after.min(remaining_std(exp, Utc::now())),
        Some(_) => after.min(RETRY_CAP),
        None => after,
    };
    // `remaining_std` is zero at/after expiry; never replace backoff with 0.
    bounded.max(RETRY_BASE)
}

/// `400`/`401`/`403` are terminal. `429`/`408`/`425`/`5xx` stay on the retry
/// backoff so a rate-limit cannot permanently stop `current()`.
async fn require_success(resp: reqwest::Response) -> Result<reqwest::Response, RefreshError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = parse_retry_after(&resp);
    let body = resp.text().await.unwrap_or_default();
    Err(RefreshError {
        error: anyhow::anyhow!("OIDC endpoint rejected request ({status}): {body}"),
        new_refresh_token: None,
        terminal: is_terminal_auth_status(status),
        retry_after,
    })
}

fn remaining_std(exp: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    (exp - now).to_std().unwrap_or(Duration::ZERO)
}

fn datetime_plus(dt: DateTime<Utc>, d: Duration) -> Option<DateTime<Utc>> {
    chrono::TimeDelta::from_std(d)
        .ok()
        .and_then(|td| dt.checked_add_signed(td))
}

fn datetime_minus(dt: DateTime<Utc>, d: Duration) -> Option<DateTime<Utc>> {
    chrono::TimeDelta::from_std(d)
        .ok()
        .and_then(|td| dt.checked_sub_signed(td))
}

fn datetime_offset(dt: DateTime<Utc>, secs: f64) -> Option<DateTime<Utc>> {
    if !secs.is_finite() {
        return None;
    }
    let millis = secs * 1000.0;
    if !(i64::MIN as f64..=i64::MAX as f64).contains(&millis) {
        return None;
    }
    dt.checked_add_signed(chrono::TimeDelta::milliseconds(millis.round() as i64))
}

#[cfg(test)]
pub(crate) fn refresh_count(outcome: &str) -> u64 {
    REFRESH_TOTAL.with_label_values(&[outcome]).get()
}

#[cfg(test)]
pub(crate) fn duration_sample_count() -> u64 {
    REFRESH_DURATION.get_sample_count()
}

#[cfg(test)]
pub(crate) fn lead_sample_count() -> u64 {
    REFRESH_LEAD.get_sample_count()
}

#[cfg(test)]
pub(crate) fn lead_sample_sum() -> f64 {
    REFRESH_LEAD.get_sample_sum()
}

#[cfg(test)]
pub(crate) fn jitter_sample_count() -> u64 {
    REFRESH_JITTER.get_sample_count()
}

#[cfg(test)]
pub(crate) fn lock_metrics() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[path = "proactive_tests.rs"]
mod tests;
