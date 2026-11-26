//! Process-wide shared `reqwest::Client`s for sampling.
//!
//! Safe to share because the builders take no config-derived input: auth,
//! extra headers, base URL, and User-Agent are applied per-request in
//! `SamplingClient::post`. Stale connections are bounded by h2 keepalive
//! (15s ping / 5s timeout while idle), 90s idle-pool eviction, and the
//! pool-less HTTP/1.1 first-retry rebuild; connections whose per-session
//! runtime died are discarded by hyper's checkout ready-check, with the
//! retry loop covering the rest.
//!
//! Wire behavior is pinned by the `shared_http_wire` and
//! `shared_http_kill_switch` binaries. Extra roots: `ECHO_AGENT_EXTRA_CA_BUNDLE`.

use std::sync::OnceLock;
use std::time::Duration;

static SHARED_H2: OnceLock<reqwest::Client> = OnceLock::new();
static SHARED_HTTP1: OnceLock<reqwest::Client> = OnceLock::new();

/// Kill switch: `ECHO_AGENT_SAMPLER_SHARED_CLIENT=0` (or `false`, any case)
/// restores the old behavior of building a fresh `reqwest::Client` per
/// `SamplingClient`. Resolved once per process: the environment cannot
/// change externally after spawn, and latching keeps the rollback state
/// consistent with the read-once pool knobs.
fn sharing_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| {
        let disabled = match std::env::var("ECHO_AGENT_SAMPLER_SHARED_CLIENT") {
            Ok(v) => v == "0" || v.eq_ignore_ascii_case("false"),
            Err(_) => false,
        };
        if disabled {
            tracing::info!(
                "sampler HTTP client sharing disabled via ECHO_AGENT_SAMPLER_SHARED_CLIENT"
            );
        }
        disabled
    })
}

/// Clone the shared client out of `cell`, building it on first use. Build
/// failures are not cached: on `Err` the cell stays empty and the next call
/// retries. A racing loser's freshly built client is simply dropped.
fn shared(
    cell: &OnceLock<reqwest::Client>,
    build: fn() -> Result<reqwest::Client, reqwest::Error>,
    disabled: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    if disabled {
        return build();
    }
    if let Some(client) = cell.get() {
        return Ok(client.clone());
    }
    let built = build()?;
    Ok(cell.get_or_init(|| built).clone())
}

/// Shared HTTP/2 sampling client (connection pooling + h2 keepalive).
pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    shared(&SHARED_H2, build_http_client, sharing_disabled())
}

/// Shared HTTP/1.1 fallback client. Pool-less by construction, so sharing it
/// is behaviorally identical to building a fresh one.
pub(crate) fn client_http1() -> Result<reqwest::Client, reqwest::Error> {
    shared(&SHARED_HTTP1, build_http_client_http1, sharing_disabled())
}

/// Build a `reqwest::Client` for sampling with HTTP/2 + connection pooling.
/// Env knobs are read once, when the shared client is first built.
fn build_http_client() -> Result<reqwest::Client, reqwest::Error> {
    let pool_max_idle: usize = std::env::var("ECHO_AGENT_POOL_MAX_IDLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let pool_idle_timeout_secs: u64 = std::env::var("ECHO_AGENT_POOL_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90);
    let connect_timeout_secs: u64 = std::env::var("ECHO_AGENT_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    echo_agent_extra_ca::build_reqwest_client(|builder| {
        builder
            // Sampling endpoints are explicit credential-bearing origins.
            // Never replay their requests through an implicit redirect.
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(pool_max_idle)
            .pool_idle_timeout(Duration::from_secs(pool_idle_timeout_secs))
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(15))
            .http2_keep_alive_timeout(Duration::from_secs(5))
            .http2_keep_alive_while_idle(true)
    })
}

/// Build a `reqwest::Client` constrained to HTTP/1.1 with pooling disabled.
/// Used as a fallback after HTTP/2 transport failures.
fn build_http_client_http1() -> Result<reqwest::Client, reqwest::Error> {
    let connect_timeout_secs: u64 = std::env::var("ECHO_AGENT_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    echo_agent_extra_ca::build_reqwest_client(|builder| {
        builder
            .redirect(reqwest::redirect::Policy::none())
            .http1_only()
            .pool_max_idle_per_host(0)
            .pool_idle_timeout(Duration::from_secs(0))
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .tcp_nodelay(true)
    })
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::shared;

    static BUILD_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Fails on the first call (a real `reqwest::Error`, no I/O), then builds.
    fn flaky_build() -> Result<reqwest::Client, reqwest::Error> {
        if BUILD_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(reqwest::Proxy::all("not a proxy url").unwrap_err());
        }
        reqwest::Client::builder().build()
    }

    #[test]
    fn shared_does_not_cache_build_failures() {
        static CELL: OnceLock<reqwest::Client> = OnceLock::new();
        assert!(shared(&CELL, flaky_build, false).is_err());
        assert!(CELL.get().is_none(), "failure must leave the cell empty");
        assert!(shared(&CELL, flaky_build, false).is_ok());
        assert!(CELL.get().is_some(), "success must populate the cell");
        assert!(shared(&CELL, flaky_build, false).is_ok());
        assert_eq!(
            BUILD_CALLS.load(Ordering::SeqCst),
            2,
            "third call must reuse the cached client, not rebuild"
        );
    }

    #[test]
    fn shared_disabled_bypasses_cell() {
        static CELL: OnceLock<reqwest::Client> = OnceLock::new();
        assert!(shared(&CELL, || reqwest::Client::builder().build(), true).is_ok());
        assert!(
            CELL.get().is_none(),
            "disabled mode must never touch the cell"
        );
    }
}
