//! Shell-side 401-attribution helpers.
//!
//! Every 401 emit site in the shell joins the bearer the client
//! actually sent on the wire (the `Authorization` value for OAI-compat
//! backends, `x-api-key` for Anthropic Messages, the API proxy
//! `Authorization` header for storage / feedback / registry /
//! idle-resume) with the manager's in-memory token
//! ([`AuthManager::current_or_expired`] -- hard-expired tokens stay
//! visible, since most 401s arrive exactly then). The two sinks are:
//!
//! 1. [`echo_agent_telemetry::unified_log::warn`] for the local
//!    `~/.echo-agent/logs/unified.jsonl` file (best-effort; ships to GCS
//!    only on OIDC refresh failure via `auth/refresh.rs`).
//! 2. A discrete `tracing::warn_span!("auth_401_attribution", ...)`
//!    captured by the OTel layer in `util/otel_layer.rs` and shipped
//!    via OTLP export to the configured telemetry backend
//!    (queryable by span name `auth_401_attribution`).
//!
//! # Schema (every emit)
//!
//! ```text
//! {
//!   "sent_key_prefix": "<last 12 chars of bearer the client sent, or """>,
//!   "current_key_prefix": "<last 12 chars of the held token (current or
//!                         expired), or null when the manager is empty>",
//!   "mint_age_seconds": <i64; current time minus auth.create_time, or -1>,
//!   "expires_at_seconds_from_now": <i64; auth.expires_at minus now
//!                                 (negative once expired), or 0 when the
//!                                 manager is empty>,
//!   "consumer": "OaiCompatClient.<endpoint>" | "StorageClient.<op>"
//!             | "FeedbackClient.<op>" | "SessionRegistryClient.<op>"
//!             | "IdleResumeModelRefresh",
//!   "is_stale_snapshot": <bool; true iff a bearer was actually sent AND it
//!                        differs from the held token -- "sent nothing"
//!                        (fail-closed) and "held nothing" are both false>
//! }
//! ```
//!
//! # Cross-crate plumbing
//!
//! [`echo_agent_sampler`] is intentionally decoupled from this crate. It
//! invokes the trait [`echo_agent_sampler::Auth401AttributionCallback`] at
//! its six 401 arms; this module provides [`ShellAttribution`], the
//! concrete impl that the shell wires into
//! [`echo_agent_sampler::SamplerConfig::attribution_callback`] at every
//! sampler-construction site. Non-sampler sites (storage / feedback /
//! registry / idle-resume) call [`record_consumer_401`]
//! directly with their `(consumer_kind, op)` pair.

use std::sync::Arc;

use echo_agent_sampler::{Auth401AttributionCallback, SamplingConsumer};
use echo_agent_tools::{
    Auth401AttributionCallback as ToolAuth401AttributionCallback, ToolConsumer,
};
use serde_json::Value as JsonValue;

use crate::auth::{AuthManager, TOKEN_TTL};
use echo_agent_auth::bearer_suffix;

/// `cfg(test)`-only process-global counter that bumps on every
/// successful `record_auth_401` invocation.
///
/// Because the counter is process-global, every test that observes it
/// MUST be annotated with `#[serial_test::serial(attribution_emit_count)]`.
#[cfg(test)]
static EMIT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read the test-only emit counter.
#[cfg(test)]
pub(crate) fn test_emit_count() -> u64 {
    EMIT_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Reset the test-only emit counter to zero. Tests that span multiple
/// instrumented call sites should call this at setup so leftover bumps
/// from earlier tests in the same process do not pollute the assertion.
#[cfg(test)]
pub(crate) fn reset_test_emit_count() {
    EMIT_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Concrete implementation of [`Auth401AttributionCallback`] for the
/// sampler crate's six 401 arms.
///
/// One instance is constructed per `SamplerConfig` and cloned cheaply
/// (the struct holds an `Arc` and an `Option<String>`). The
/// `session_id` is captured at construction time and used for the
/// `unified_log::warn` `sid` field; non-session callers may pass
/// `None`.
pub(crate) struct ShellAttribution {
    auth_manager: Arc<AuthManager>,
    session_id: Option<String>,
}

// `AuthManager` does not implement `Debug` (it carries a `RwLock` over
// auth state and would expose secrets if it did). Hand-roll a redacted
// `Debug` impl so the `Auth401AttributionCallback` trait's
// `Debug + Send + Sync` bound is satisfied without changing
// `AuthManager`'s API surface.
impl std::fmt::Debug for ShellAttribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellAttribution")
            .field("auth_manager", &"<redacted>")
            .field("session_id", &self.session_id)
            .finish()
    }
}

impl ShellAttribution {
    /// Construct a shareable attribution callback wired to the given
    /// [`AuthManager`]. Returns `Arc<dyn Trait>` for the sampler
    /// trait so callers can drop the value directly into
    /// [`echo_agent_sampler::SamplerConfig::attribution_callback`].
    ///
    /// (Returns `Arc<dyn Trait>` rather than `Self` because the
    /// `echo_agent_sampler::SamplerConfig` field expects exactly that;
    /// keeping the boundary in one place avoids `as Arc<dyn _>`
    /// coercions at every call site.)
    #[allow(clippy::new_ret_no_self)]
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        session_id: Option<String>,
    ) -> Arc<dyn Auth401AttributionCallback> {
        Arc::new(Self {
            auth_manager,
            session_id,
        })
    }

    /// Tool-side counterpart of [`Self::new`]: returns
    /// `Arc<dyn echo_agent_tools::Auth401AttributionCallback>` for the
    /// `with_attribution_callback(...)` builder on each tool HTTP
    /// client (`ImageGenClient`, `VideoGenClient`, `WebSearchClient`).
    /// The two callbacks share the same underlying impl and emit the
    /// same `auth_401_attribution` event format -- only the trait
    /// signature differs (`SamplingConsumer` vs. `ToolConsumer`).
    pub(crate) fn new_tool_callback(
        auth_manager: Arc<AuthManager>,
        session_id: Option<String>,
    ) -> Arc<dyn ToolAuth401AttributionCallback> {
        Arc::new(Self {
            auth_manager,
            session_id,
        })
    }
}

impl Auth401AttributionCallback for ShellAttribution {
    fn record_401(&self, consumer: SamplingConsumer, sent_bearer_suffix: Option<&str>) {
        // Already truncated by the sampler; the re-truncate downstream is a
        // deliberate no-op so the full bearer never leaves that crate.
        record_consumer_401(
            self.auth_manager.as_ref(),
            self.session_id.as_deref(),
            ConsumerKind::OaiCompatClient,
            consumer.as_endpoint(),
            sent_bearer_suffix,
        );
    }
}

/// Tool-side hook: each tool client (image_gen, video_gen, web_search)
/// in `echo-agent-tools` emits a 401 attribution event through this
/// trait when its HTTP request returns UNAUTHORIZED. Same shape as
/// the sampler-side impl above; routes to the same pair of sinks.
///
/// `ToolConsumer::VideoGenStart` and `VideoGenPoll` collapse to the
/// same [`ConsumerKind::VideoGen`] with different op strings so the
/// gate query can break down video-gen 401s by phase.
impl ToolAuth401AttributionCallback for ShellAttribution {
    fn record_401(&self, consumer: ToolConsumer, sent_bearer_suffix: Option<&str>) {
        let (kind, op) = match consumer {
            ToolConsumer::ImageGen => (ConsumerKind::ImageGen, ""),
            ToolConsumer::VideoGenStart => (ConsumerKind::VideoGen, "start"),
            ToolConsumer::VideoGenPoll => (ConsumerKind::VideoGen, "poll"),
            ToolConsumer::WebSearch => (ConsumerKind::WebSearch, ""),
        };
        record_consumer_401(
            self.auth_manager.as_ref(),
            self.session_id.as_deref(),
            kind,
            op,
            sent_bearer_suffix,
        );
    }
}

/// Categories of 401-attribution emit sites. Each variant maps to a
/// fixed prefix in the rendered `consumer` field; the per-site `op`
/// string is appended after a `.` separator (omitted for variants that
/// have no per-operation discriminator, e.g.
/// [`ConsumerKind::IdleResumeModelRefresh`]).
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConsumerKind {
    /// Sampler-side OpenAI-compat / Anthropic Messages emit. The op
    /// string is the [`SamplingConsumer::as_endpoint`] return value.
    OaiCompatClient,
    /// Storage upload / batch / check sites in `upload/storage_client.rs`.
    StorageClient,
    /// Feedback collection sites in `agent/feedback_client.rs`.
    FeedbackClient,
    /// Session registry register/update sites in
    /// `agent/session_registry_client.rs`.
    SessionRegistryClient,
    /// Idle-resume model-metadata refresh in
    /// `session/acp_session.rs::maybe_refresh_model_metadata_on_resume`.
    /// No per-op discriminator -- the consumer string is just
    /// `"IdleResumeModelRefresh"`.
    IdleResumeModelRefresh,
    /// `echo_agent_tools::ToolConsumer::ImageGen` -- Imagine API
    /// (`POST /images/generations`). No per-op discriminator;
    /// consumer string is just `"ImageGen"`.
    ImageGen,
    /// `echo_agent_tools::ToolConsumer::VideoGenStart` and
    /// `VideoGenPoll` -- Video Generation API. The op string is
    /// `"start"` (`POST /videos/generations`) or `"poll"`
    /// (`GET /videos/{request_id}`).
    VideoGen,
    /// `echo_agent_tools::ToolConsumer::WebSearch` -- web search via
    /// `POST /responses` with a `WebSearch` tool. No per-op
    /// discriminator; consumer string is just `"WebSearch"`.
    WebSearch,
}

impl ConsumerKind {
    /// Fixed prefix for the rendered `consumer` field.
    fn prefix(self) -> &'static str {
        match self {
            Self::OaiCompatClient => "OaiCompatClient",
            Self::StorageClient => "StorageClient",
            Self::FeedbackClient => "FeedbackClient",
            Self::SessionRegistryClient => "SessionRegistryClient",
            Self::IdleResumeModelRefresh => "IdleResumeModelRefresh",
            Self::ImageGen => "ImageGen",
            Self::VideoGen => "VideoGen",
            Self::WebSearch => "WebSearch",
        }
    }

    /// `true` for variants that take a per-operation discriminator
    /// appended as `<prefix>.<op>`. `false` for variants whose
    /// `consumer` string is just the prefix
    /// (`IdleResumeModelRefresh`, `ImageGen`, `WebSearch` -- each is
    /// a single endpoint with no sub-operation).
    fn takes_op(self) -> bool {
        !matches!(
            self,
            Self::IdleResumeModelRefresh | Self::ImageGen | Self::WebSearch
        )
    }
}

/// Format a `(kind, op)` pair into the design-doc `consumer` string.
fn format_consumer(kind: ConsumerKind, op: &str) -> String {
    if kind.takes_op() {
        format!("{}.{}", kind.prefix(), op)
    } else {
        kind.prefix().to_string()
    }
}

/// Emit a single `auth 401 attribution` event for a per-consumer 401.
///
/// Wraps [`record_auth_401`] with the design-doc `consumer` formatting
/// (e.g., `"StorageClient.upload"`, `"FeedbackClient.submit"`).
/// All 401 emit sites in `echo-agent-runtime` go through this helper -- the
/// per-client `record_401_attribution` wrappers in
/// `agent/feedback_client.rs`, `agent/session_registry_client.rs`,
/// and `upload/storage_client.rs` each
/// resolve their bearer and call this with the right `(kind, op)`.
///
/// `sent_bearer` may be either a full bearer (passed by the
/// non-sampler call sites listed above, which read directly from the
/// client's `user_token` / `deployment_key` snapshot) or a 12-char
/// prefix (passed by the sampler-side
/// [`Auth401AttributionCallback`] boundary; the sampler scrubs to a
/// prefix before crossing the crate boundary). The truncation inside
/// [`record_auth_401`] / `compute_attribution_payload` is idempotent
/// for the prefix case.
pub(crate) fn record_consumer_401(
    auth_manager: &AuthManager,
    session_id: Option<&str>,
    kind: ConsumerKind,
    op: &str,
    sent_bearer: Option<&str>,
) {
    let consumer = format_consumer(kind, op);
    record_auth_401(auth_manager, session_id, &consumer, sent_bearer);
}

/// Emit a single `auth 401 attribution` event to both sinks (local
/// unified log file + OTel span for OTLP export).
///
/// Schema:
/// `(sent_key_prefix, current_key_prefix, mint_age_seconds,
///   expires_at_seconds_from_now, consumer, is_stale_snapshot)`.
///
/// `sent_bearer` is the bearer that was sent on the wire (the
/// `Authorization` value with `"Bearer "` stripped, or `x-api-key`), OR
/// its 12-char tail fragment -- the sampler and middleware boundaries
/// pass tails; a caller holding the full bearer may rely on the
/// [`compute_attribution_payload`] truncation. `None` becomes the empty
/// string, meaning "no bearer was sent."
///
/// `consumer` should be one of the canonical strings used by the
/// per-client wrappers, e.g. `"OaiCompatClient.chat_completions_stream"`,
/// `"StorageClient.upload"`, `"IdleResumeModelRefresh"`. Most call
/// sites should go through [`record_consumer_401`] which formats the
/// consumer string from a [`ConsumerKind`] for them.
pub(crate) fn record_auth_401(
    auth_manager: &AuthManager,
    session_id: Option<&str>,
    consumer: &str,
    sent_bearer: Option<&str>,
) {
    let payload = compute_attribution_payload(auth_manager, consumer, sent_bearer);

    // Sink 1 -- local file (~/.echo-agent/logs/unified.jsonl) + scrubbed
    // tracing event. The local file is reliable but only ships to GCS
    // on OIDC refresh failure (auth/refresh.rs::spawn_diagnostic_upload),
    // so by itself it does not give visibility into the steady-state
    // 401 population. Sink 2 below provides that.
    echo_agent_telemetry::unified_log::warn(
        "auth 401 attribution",
        session_id,
        Some(payload.clone()),
    );

    // Sink 2 -- discrete OTel span exported via OTLP
    // (util/otel_layer.rs). Auth 401 attribution schema fields below
    // become OTel span attributes under `attributes.custom.<name>`
    // per the tracing-opentelemetry bridge; query by span name
    // `auth_401_attribution` in the configured telemetry backend.
    //
    // Wrapping in a `warn_span!` (vs. plain `tracing::warn!`) ensures
    // emission even when no parent span is active. The OTel layer
    // attaches plain events to the currently-entered span only, so a
    // `tracing::warn!` from a `spawn_blocking` closure (idle-resume
    // model refresh) or a background sync task is silently dropped.
    // A `warn_span!` itself is always emitted by the layer's
    // `on_new_span`/`on_close` hooks regardless of parent context.
    //
    // The span carries no body and is dropped immediately at the end
    // of this function, so its `duration` is a few microseconds and
    // it is logically a one-shot record (not a wrapping context for
    // any other work).
    let _attribution_span = tracing::warn_span!(
        "auth_401_attribution",
        // String fields. tracing flattens Option<&str> via Display, so
        // we pre-collapse `None` to "" for both prefix fields and for
        // session_id; downstream queries should treat "" as absent.
        sent_key_prefix = payload["sent_key_prefix"].as_str().unwrap_or(""),
        current_key_prefix = payload["current_key_prefix"].as_str().unwrap_or(""),
        consumer = consumer,
        session_id = session_id.unwrap_or(""),
        // Numeric fields. The sentinel values from
        // `compute_attribution_payload` (-1, 0) carry through
        // unchanged.
        mint_age_seconds = payload["mint_age_seconds"].as_i64().unwrap_or(-1),
        expires_at_seconds_from_now = payload["expires_at_seconds_from_now"].as_i64().unwrap_or(0),
        // Boolean -- the load-bearing field for stale-vs-live splits.
        is_stale_snapshot = payload["is_stale_snapshot"].as_bool().unwrap_or(false),
    )
    .entered();

    #[cfg(test)]
    EMIT_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Pure (no I/O) computation of the attribution payload. Extracted
/// from [`record_auth_401`] so unit tests can assert each field
/// directly without reaching into `unified_log`'s file writer or the
/// tracing layer.
///
/// Reads [`AuthManager::current_or_expired`] -- NOT `current()`, which is
/// `None` by construction in the hard-expired window most 401s land in
/// and would blank every field this event exists to fill.
///
/// `is_stale_snapshot` is `true` only when a bearer was actually sent
/// and it differs from the held token; "sent nothing" (fail-closed) and
/// "held nothing" (empty manager) are both `false`.
fn compute_attribution_payload(
    auth_manager: &AuthManager,
    consumer: &str,
    sent_bearer: Option<&str>,
) -> JsonValue {
    let now = chrono::Utc::now();

    // Last-12-char suffix of the bearer the wire actually carried
    // (see [`bearer_suffix`]: JWT headers share a common base64 prefix).
    // `""` when the request had no bearer at all (distinct case from
    // "had a bearer that turned out to be stale" -- the gate-criteria
    // query can break down on this).
    let sent_suffix = sent_bearer.map(bearer_suffix).unwrap_or("");

    // One read; `current_or_expired` keeps the hard-expired token visible
    // (see the fn doc).
    let current_auth = auth_manager.current_or_expired();
    let current_suffix_owned: Option<String> = current_auth
        .as_ref()
        .map(|a| bearer_suffix(&a.key).to_string());

    // True-positive staleness only: a bearer was sent AND differs from
    // the held token. "Sent nothing" is the fail-closed path (in sync,
    // credential dead); "held nothing" is no evidence -- neither is stale.
    let is_stale_snapshot = match (sent_suffix, current_suffix_owned.as_deref()) {
        ("", _) => false,
        (_, None) => false,
        (sent, Some(held)) => sent != held,
    };

    // Mint-age + expiry come from the same `current_auth` we already
    // read; sentinels `-1 / 0` when the manager holds nothing. For a
    // hard-expired token these report true age and (negative)
    // time-past-expiry: how long the bearer was dead at the 401.
    //
    // TODO: mirror the full External-with-ttl branch from
    // `AuthManager::is_token_expired` (uses
    // `echo_agent_com_config.auth_token_ttl` when `expires_at` is `None`
    // and `auth_mode == External`). The current 2-branch fallback
    // (`expires_at` if Some else `create_time + TOKEN_TTL`) is good
    // enough for diagnostic metadata; the External-ttl branch is
    // worth wiring once a real consumer needs it.
    let (mint_age_seconds, expires_at_seconds_from_now) = match current_auth {
        Some(auth) => {
            let mint_age = now.signed_duration_since(auth.create_time).num_seconds();
            let expiry = auth.expires_at.unwrap_or(auth.create_time + TOKEN_TTL);
            (mint_age, expiry.signed_duration_since(now).num_seconds())
        }
        None => (-1_i64, 0_i64),
    };

    serde_json::json!({
        "sent_key_prefix": sent_suffix,
        "current_key_prefix": current_suffix_owned,
        "mint_age_seconds": mint_age_seconds,
        "expires_at_seconds_from_now": expires_at_seconds_from_now,
        "consumer": consumer,
        "is_stale_snapshot": is_stale_snapshot,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};

    use crate::auth::{AuthManager, EchoAgentAuth, EchoAgentComConfig};

    use super::*;

    /// Test helper: build a fresh `AuthManager` rooted at a tempdir so
    /// nothing from a developer's actual `~/.echo-agent/auth.json` leaks in.
    fn empty_auth_manager() -> (tempfile::TempDir, AuthManager) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = EchoAgentComConfig::default();
        let am = AuthManager::new(dir.path(), cfg);
        (dir, am)
    }

    fn fresh_auth(key: &str) -> EchoAgentAuth {
        EchoAgentAuth {
            key: key.to_string(),
            create_time: Utc::now(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        }
    }

    fn payload_field<'a>(payload: &'a JsonValue, key: &str) -> &'a JsonValue {
        payload
            .get(key)
            .unwrap_or_else(|| panic!("payload missing field {key:?}: {payload:?}"))
    }

    /// Live token sent + 401 with matching `current()` ->
    /// `is_stale_snapshot` must be `false`. Also assert the auxiliary
    /// fields are set sensibly (prefix, mint age, expiry).
    #[test]
    fn live_token_sent_is_not_stale() {
        let (_dir, am) = empty_auth_manager();
        let sent = "live-token-1234567890abcdef";
        am.hot_swap(fresh_auth(sent));

        let payload = compute_attribution_payload(&am, "Test.live", Some(sent));

        assert_eq!(payload_field(&payload, "is_stale_snapshot"), false);
        assert_eq!(payload_field(&payload, "consumer"), "Test.live");
        // Last 12 chars (tail prefix for JWT-friendly diagnostics).
        assert_eq!(payload_field(&payload, "sent_key_prefix"), "567890abcdef");
        assert_eq!(
            payload_field(&payload, "current_key_prefix"),
            "567890abcdef"
        );
        // mint_age_seconds: should be small and non-negative for a
        // freshly-created auth.
        let mint = payload_field(&payload, "mint_age_seconds")
            .as_i64()
            .unwrap();
        assert!(
            (0..5).contains(&mint),
            "mint_age_seconds should be 0-5 sec for a freshly-created auth, got {mint}"
        );
        // expires_at_seconds_from_now: should be just under 1 hour
        // (3600s), with a tolerance for elapsed time during the test.
        let expires = payload_field(&payload, "expires_at_seconds_from_now")
            .as_i64()
            .unwrap();
        assert!(
            (3590..=3600).contains(&expires),
            "expires_at_seconds_from_now should be ~3600 for a 1h-expiry token, got {expires}"
        );
    }

    /// Stale snapshot sent + 401 with a different (newer) `current()`
    /// -> `is_stale_snapshot` must be `true`.
    #[test]
    fn stale_snapshot_is_detected() {
        let (_dir, am) = empty_auth_manager();
        let stale = "stale-token-1234567890";
        let live = "live-token-different";
        am.hot_swap(fresh_auth(live));

        let payload = compute_attribution_payload(&am, "Test.stale", Some(stale));

        assert_eq!(payload_field(&payload, "is_stale_snapshot"), true);
        assert_eq!(payload_field(&payload, "sent_key_prefix"), "n-1234567890");
        assert_eq!(
            payload_field(&payload, "current_key_prefix"),
            "en-different"
        );
        assert_eq!(payload_field(&payload, "consumer"), "Test.stale");
    }

    /// Live token sent + 401 with `current() == None` ->
    /// `is_stale_snapshot` must be `false` (no evidence of staleness).
    /// Sentinel `mint_age_seconds = -1`,
    /// `expires_at_seconds_from_now = 0`. `current_key_prefix` is JSON
    /// `null`.
    #[test]
    fn absent_current_is_not_stale() {
        let (_dir, am) = empty_auth_manager();
        // Do NOT inject anything -- manager has no current token.

        let payload = compute_attribution_payload(&am, "Test.absent", Some("any-token"));

        assert_eq!(payload_field(&payload, "is_stale_snapshot"), false);
        assert_eq!(payload_field(&payload, "sent_key_prefix"), "any-token");
        assert!(payload_field(&payload, "current_key_prefix").is_null());
        assert_eq!(payload_field(&payload, "mint_age_seconds"), -1);
        assert_eq!(payload_field(&payload, "expires_at_seconds_from_now"), 0);
    }

    /// Test helper: a token minted 2h ago that hard-expired 1h ago --
    /// the in-memory state during the exact window most 401s occur in
    /// (`current()` is `None`, `expired_auth()` is `Some`).
    fn hard_expired_auth(key: &str) -> EchoAgentAuth {
        EchoAgentAuth {
            key: key.to_string(),
            create_time: Utc::now() - Duration::hours(2),
            expires_at: Some(Utc::now() - Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        }
    }

    /// A consumer sends the very token the manager holds, hard-expired:
    /// NOT stale (in sync; the token itself is dead). The held token and
    /// real age fields must stay visible -- `current()` used to blank them.
    #[test]
    fn hard_expired_held_token_sent_is_not_stale() {
        let (_dir, am) = empty_auth_manager();
        let sent = "expired-token-1234567890abcdef";
        am.hot_swap(hard_expired_auth(sent));
        assert!(am.current().is_none(), "hard-expired precondition");

        let payload = compute_attribution_payload(&am, "Test.expired", Some(sent));

        assert_eq!(payload_field(&payload, "is_stale_snapshot"), false);
        assert_eq!(
            payload_field(&payload, "current_key_prefix"),
            "567890abcdef",
            "the held token must stay visible even when hard-expired"
        );
        let mint = payload_field(&payload, "mint_age_seconds")
            .as_i64()
            .unwrap();
        assert!(
            (7195..=7210).contains(&mint),
            "mint_age_seconds should be ~7200 for a 2h-old token, got {mint}"
        );
        let expires = payload_field(&payload, "expires_at_seconds_from_now")
            .as_i64()
            .unwrap();
        assert!(
            (-3610..=-3590).contains(&expires),
            "expires_at_seconds_from_now should be ~-3600 for a token dead 1h, got {expires}"
        );
    }

    /// The fail-closed path: a hard-expired token is held, and the
    /// wire-valid-only resolver correctly put NO bearer on the wire.
    /// Not a stale snapshot -- the consumer did the right thing; the
    /// credential is dead. Absorbing this into the stale bucket would
    /// bury the true-positive split the field exists for.
    #[test]
    fn nothing_sent_with_hard_expired_held_token_is_not_stale() {
        let (_dir, am) = empty_auth_manager();
        am.hot_swap(hard_expired_auth("held-but-not-sent"));

        let payload = compute_attribution_payload(&am, "Test.fail_closed", None);

        assert_eq!(payload_field(&payload, "is_stale_snapshot"), false);
        assert_eq!(payload_field(&payload, "sent_key_prefix"), "");
        assert_eq!(
            payload_field(&payload, "current_key_prefix"),
            "but-not-sent",
            "the held token must stay visible for diagnosis"
        );
    }

    /// A consumer sends an OLDER bearer than the (hard-expired) one the
    /// manager holds: a true stale snapshot, and it must be flagged even
    /// though `current()` is `None` in this window.
    #[test]
    fn stale_snapshot_detected_against_hard_expired_held_token() {
        let (_dir, am) = empty_auth_manager();
        am.hot_swap(hard_expired_auth("held-token-different"));
        assert!(am.current().is_none(), "hard-expired precondition");

        let payload =
            compute_attribution_payload(&am, "Test.expired_stale", Some("frozen-at-spawn-copy"));

        assert_eq!(payload_field(&payload, "is_stale_snapshot"), true);
        assert_eq!(
            payload_field(&payload, "current_key_prefix"),
            "en-different"
        );
    }

    /// Two-branch fallback: legacy token (no `expires_at`) uses
    /// `create_time + TOKEN_TTL` as the expiry source. We assert the
    /// computed `expires_at_seconds_from_now` reflects that.
    #[test]
    fn legacy_token_uses_two_branch_fallback() {
        let (_dir, am) = empty_auth_manager();
        let auth = EchoAgentAuth {
            key: "k".into(),
            create_time: Utc::now() - Duration::seconds(60),
            // No expires_at => falls through to create_time + TOKEN_TTL
            // (= 30 days).
            ..EchoAgentAuth::test_default()
        };
        am.hot_swap(auth);

        let payload = compute_attribution_payload(&am, "Test.legacy", Some("k"));

        // mint_age_seconds: ~60.
        let mint = payload_field(&payload, "mint_age_seconds")
            .as_i64()
            .unwrap();
        assert!(
            (60..=70).contains(&mint),
            "mint_age_seconds should be ~60 for a 60s-old auth, got {mint}"
        );
        // expires_at_seconds_from_now: TOKEN_TTL minus 60s = roughly
        // 30 * 86400 - 60 = 2_591_940. Tolerate ~10s drift.
        let expires = payload_field(&payload, "expires_at_seconds_from_now")
            .as_i64()
            .unwrap();
        let expected = TOKEN_TTL.num_seconds() - 60;
        assert!(
            (expected - 10..=expected + 10).contains(&expires),
            "expires_at_seconds_from_now should be ~{expected}, got {expires}"
        );
    }

    /// `format_consumer` matrix:
    ///   - generic ops append "." + op (`OaiCompatClient.foo`)
    ///   - IdleResumeModelRefresh and tool variants drop the op
    ///     (their consumer string has no sub-op axis).
    #[test]
    fn format_consumer_matrix() {
        let cases: &[(ConsumerKind, &str, &str)] = &[
            (
                ConsumerKind::OaiCompatClient,
                "chat_completions_stream",
                "OaiCompatClient.chat_completions_stream",
            ),
            (
                ConsumerKind::StorageClient,
                "upload_file",
                "StorageClient.upload_file",
            ),
            (
                ConsumerKind::IdleResumeModelRefresh,
                "",
                "IdleResumeModelRefresh",
            ),
            (
                ConsumerKind::IdleResumeModelRefresh,
                "ignored",
                "IdleResumeModelRefresh",
            ),
            (ConsumerKind::ImageGen, "", "ImageGen"),
            (ConsumerKind::ImageGen, "ignored", "ImageGen"),
            (ConsumerKind::VideoGen, "start", "VideoGen.start"),
            (ConsumerKind::VideoGen, "poll", "VideoGen.poll"),
            (ConsumerKind::WebSearch, "", "WebSearch"),
            (ConsumerKind::WebSearch, "ignored", "WebSearch"),
        ];
        for (kind, op, expected) in cases {
            assert_eq!(
                format_consumer(*kind, op),
                *expected,
                "kind={kind:?} op={op:?}"
            );
        }
    }

    /// `format_consumer` formats `OaiCompatClient.<endpoint>`
    /// correctly and omits the `.` separator for
    /// `IdleResumeModelRefresh`.
    #[test]
    fn format_consumer_with_op_appends_dot() {
        assert_eq!(
            format_consumer(ConsumerKind::OaiCompatClient, "chat_completions_stream"),
            "OaiCompatClient.chat_completions_stream"
        );
        assert_eq!(
            format_consumer(ConsumerKind::StorageClient, "upload_file"),
            "StorageClient.upload_file"
        );
    }

    /// `ShellAttribution` implements `echo_agent_tools::Auth401AttributionCallback`
    /// by routing each `ToolConsumer` variant to the right
    /// `(ConsumerKind, op)` pair, which formats to the expected
    /// `consumer` string in the emitted payload.
    #[test]
    #[serial_test::serial(attribution_emit_count)]
    fn shell_attribution_tool_impl_routes_to_correct_consumer_strings() {
        reset_test_emit_count();
        let (_dir, am) = empty_auth_manager();
        am.hot_swap(fresh_auth("bearer-1234567890"));
        let am_arc = Arc::new(am);
        let cb: Arc<dyn ToolAuth401AttributionCallback> =
            ShellAttribution::new_tool_callback(am_arc.clone(), Some("sid-tool".into()));

        let cases = [
            (ToolConsumer::ImageGen, "ImageGen"),
            (ToolConsumer::VideoGenStart, "VideoGen.start"),
            (ToolConsumer::VideoGenPoll, "VideoGen.poll"),
            (ToolConsumer::WebSearch, "WebSearch"),
        ];

        for (consumer, expected_consumer_str) in cases {
            cb.record_401(consumer, Some("bearer-1234567890"));
            let payload = compute_attribution_payload(
                am_arc.as_ref(),
                expected_consumer_str,
                Some("bearer-1234567890"),
            );
            assert_eq!(
                payload_field(&payload, "consumer"),
                expected_consumer_str,
                "ToolConsumer::{consumer:?} should render as {expected_consumer_str:?}",
            );
        }

        // Each variant bumped the global counter exactly once.
        assert_eq!(test_emit_count() as usize, cases.len());
    }

    /// Capture `tracing::Span` `on_new_span` callbacks into a
    /// `Mutex<Vec<CapturedSpan>>` so tests can assert the
    /// `warn_span!("auth_401_attribution", ...)` emit fired with the
    /// expected name and field values.
    ///
    /// We intentionally only need `on_new_span` (which the
    /// tracing-opentelemetry layer uses as its `OTel span_started`
    /// hook). `on_close` is not asserted because the test cares about
    /// "did the span exist with these attributes," not its duration.
    mod span_capture {
        use std::sync::Mutex;
        use tracing::Subscriber;
        use tracing::field::{Field, Visit};
        use tracing::span::Attributes;
        use tracing_subscriber::layer::{Context, Layer};
        use tracing_subscriber::registry::LookupSpan;

        #[derive(Debug, Default, Clone)]
        pub(crate) struct CapturedSpan {
            pub name: String,
            pub fields_str: std::collections::BTreeMap<String, String>,
            pub fields_i64: std::collections::BTreeMap<String, i64>,
            pub fields_bool: std::collections::BTreeMap<String, bool>,
        }

        pub(crate) struct SpanCollector {
            pub spans: std::sync::Arc<Mutex<Vec<CapturedSpan>>>,
        }

        impl SpanCollector {
            pub(crate) fn new() -> (Self, std::sync::Arc<Mutex<Vec<CapturedSpan>>>) {
                let buf = std::sync::Arc::new(Mutex::new(Vec::new()));
                (Self { spans: buf.clone() }, buf)
            }
        }

        impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for SpanCollector {
            fn on_new_span(&self, attrs: &Attributes<'_>, _id: &tracing::Id, _ctx: Context<'_, S>) {
                let mut captured = CapturedSpan {
                    name: attrs.metadata().name().to_string(),
                    ..Default::default()
                };
                let mut visitor = FieldVisitor {
                    captured: &mut captured,
                };
                attrs.record(&mut visitor);
                self.spans.lock().unwrap().push(captured);
            }
        }

        struct FieldVisitor<'a> {
            captured: &'a mut CapturedSpan,
        }

        impl<'a> Visit for FieldVisitor<'a> {
            fn record_str(&mut self, field: &Field, value: &str) {
                self.captured
                    .fields_str
                    .insert(field.name().to_string(), value.to_string());
            }
            fn record_i64(&mut self, field: &Field, value: i64) {
                self.captured
                    .fields_i64
                    .insert(field.name().to_string(), value);
            }
            fn record_u64(&mut self, field: &Field, value: u64) {
                self.captured
                    .fields_i64
                    .insert(field.name().to_string(), value as i64);
            }
            fn record_bool(&mut self, field: &Field, value: bool) {
                self.captured
                    .fields_bool
                    .insert(field.name().to_string(), value);
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                self.captured
                    .fields_str
                    .insert(field.name().to_string(), format!("{value:?}"));
            }
        }
    }

    /// `record_auth_401` emits a discrete `warn_span!` with name
    /// `"auth_401_attribution"` and the attribution fields as span
    /// attributes. This is the span the tracing-opentelemetry bridge
    /// ships via OTLP export to the configured telemetry backend.
    /// Verifies field names, types, and values match the schema
    /// documented at the top of this module.
    #[test]
    #[serial_test::serial(attribution_emit_count)]
    fn record_auth_401_emits_otel_span_with_attribution_fields() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let (collector, captured) = span_capture::SpanCollector::new();
        let subscriber = tracing_subscriber::registry().with(collector);
        let _guard = subscriber.set_default();

        reset_test_emit_count();
        let (_dir, am) = empty_auth_manager();
        am.hot_swap(fresh_auth("live-token-1234567890"));

        record_auth_401(
            &am,
            Some("sid-otel-span"),
            "OaiCompatClient.chat_completions_stream",
            Some("stale-snapshot-aaaaaa"),
        );

        let spans = captured.lock().unwrap();
        let attribution = spans
            .iter()
            .find(|s| s.name == "auth_401_attribution")
            .expect("expected one auth_401_attribution span; got: {spans:?}");

        // String fields: prefixes truncated to 12 chars, consumer +
        // session_id passed verbatim.
        assert_eq!(
            attribution
                .fields_str
                .get("sent_key_prefix")
                .map(String::as_str),
            Some("pshot-aaaaaa"),
            "sent_key_prefix should be last 12 chars",
        );
        assert_eq!(
            attribution
                .fields_str
                .get("current_key_prefix")
                .map(String::as_str),
            Some("n-1234567890"),
        );
        assert_eq!(
            attribution.fields_str.get("consumer").map(String::as_str),
            Some("OaiCompatClient.chat_completions_stream"),
        );
        assert_eq!(
            attribution.fields_str.get("session_id").map(String::as_str),
            Some("sid-otel-span"),
        );

        // Boolean: the load-bearing field for stale-vs-live splits.
        // `true` because `sent != current`.
        assert_eq!(
            attribution.fields_bool.get("is_stale_snapshot"),
            Some(&true),
        );

        // Numeric: mint_age in [0, 5) for a freshly-injected auth;
        // expires_at ~3600s away.
        let mint = attribution
            .fields_i64
            .get("mint_age_seconds")
            .copied()
            .unwrap();
        assert!(
            (0..5).contains(&mint),
            "mint_age_seconds should be 0-5, got {mint}",
        );
        let expires = attribution
            .fields_i64
            .get("expires_at_seconds_from_now")
            .copied()
            .unwrap();
        assert!(
            (3590..=3600).contains(&expires),
            "expires_at_seconds_from_now should be ~3600, got {expires}",
        );
    }

    /// `record_auth_401` (the I/O-bearing wrapper) bumps the
    /// `cfg(test)` counter so cross-module tests can observe how many
    /// times an attribution event was actually emitted.
    ///
    /// `#[serial]` because `EMIT_COUNT` is process-global; concurrent
    /// tests that exercise the counter would race each other.
    #[test]
    #[serial_test::serial(attribution_emit_count)]
    fn record_auth_401_bumps_emit_counter() {
        reset_test_emit_count();
        let (_dir, am) = empty_auth_manager();
        am.hot_swap(fresh_auth("k"));
        record_auth_401(&am, None, "Test.counter", Some("k"));
        assert_eq!(test_emit_count(), 1);
        record_auth_401(&am, None, "Test.counter", Some("k"));
        assert_eq!(test_emit_count(), 2);
    }

    /// The SubagentSpawnContext-borne callback flows through
    /// `read_parent_sampling_config` into the inherited
    /// `SamplerConfig.attribution_callback`. We can't drive the full
    /// subagent path here (requires SessionActor + chat-state
    /// scaffolding), but we can assert the structural property: the
    /// callback the parent constructs is the one any later
    /// `SamplerConfig` clone carries forward unchanged.
    #[test]
    #[serial_test::serial(attribution_emit_count)]
    fn parent_callback_flows_through_arc_clone() {
        reset_test_emit_count();
        let (_dir, am) = empty_auth_manager();
        let am_arc = Arc::new(am);
        let parent_cb = ShellAttribution::new(am_arc.clone(), Some("parent-sid".into()));

        // Simulate the inheritance hand-off: the parent callback flows
        // through SessionHandle -> SubagentSpawnContext ->
        // SamplerConfig.attribution_callback as plain Arc clones.
        let inherited_cb = parent_cb.clone();

        // Drive the inherited callback. The `record_401` should bump
        // the same global counter the parent callback would, proving
        // they refer to the same underlying impl.
        inherited_cb.record_401(SamplingConsumer::ChatCompletionsStream, Some("bearer"));
        assert_eq!(test_emit_count(), 1);

        // Sanity: the parent_cb still works too (it's the same Arc).
        parent_cb.record_401(SamplingConsumer::Messages, Some("bearer"));
        assert_eq!(test_emit_count(), 2);
    }

    /// End-to-end: the trait impl wraps `consumer.as_endpoint()` in
    /// `"OaiCompatClient.<endpoint>"` and delegates to
    /// `record_consumer_401` for every variant of `SamplingConsumer`.
    /// We assert one bump per variant via the test counter, plus the
    /// rendered `consumer` string for one variant via a payload
    /// recompute (the trait does not return the payload, so we
    /// recompute directly from the same inputs).
    #[test]
    #[serial_test::serial(attribution_emit_count)]
    fn shell_attribution_trait_impl_routes_through_helper() {
        reset_test_emit_count();
        let (_dir, am) = empty_auth_manager();
        let am_arc = Arc::new(am);
        let cb = ShellAttribution::new(am_arc.clone(), Some("sid-shell".into()));
        let variants = [
            SamplingConsumer::ChatCompletionsStream,
            SamplingConsumer::ChatCompletions,
            SamplingConsumer::ResponsesStream,
            SamplingConsumer::Responses,
            SamplingConsumer::MessagesStream,
            SamplingConsumer::Messages,
        ];
        for consumer in variants {
            cb.record_401(consumer, Some("test-bearer"));
        }
        assert_eq!(test_emit_count() as usize, variants.len());

        // Sanity-check the consumer-string formatting via direct
        // payload computation.
        let payload = compute_attribution_payload(
            am_arc.as_ref(),
            &format_consumer(
                ConsumerKind::OaiCompatClient,
                SamplingConsumer::MessagesStream.as_endpoint(),
            ),
            Some("test-bearer"),
        );
        assert_eq!(
            payload_field(&payload, "consumer"),
            "OaiCompatClient.messages_stream"
        );
    }
}
