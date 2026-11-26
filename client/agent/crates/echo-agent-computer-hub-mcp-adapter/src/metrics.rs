//! Feature-gated Prometheus metrics for the MCP adapter bridge.
//!
//! When the `metrics` cargo feature is enabled, each helper records to a
//! lazily-registered Prometheus counter / gauge / histogram. When
//! disabled (the default), every helper compiles to an empty function
//! body so the crate carries zero prometheus dependency.

#[cfg(feature = "metrics")]
mod inner {
    use prometheus::{
        Histogram, IntCounter, IntGauge, exponential_buckets, register_histogram,
        register_int_counter, register_int_gauge,
    };
    use std::sync::LazyLock;

    static MCP_CALL_DURATION_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
        register_histogram!(
            "computer_hub_mcp_call_duration_seconds",
            "MCP server response latency for tool calls.",
            exponential_buckets(0.01, 2.0, 14).expect("valid bucket params")
        )
        .expect("computer_hub_mcp_call_duration_seconds must register once")
    });

    static MCP_ERRORS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
        register_int_counter!(
            "computer_hub_mcp_errors_total",
            "Errors in the MCP adapter pipeline (transport, protocol, or serialization)."
        )
        .expect("computer_hub_mcp_errors_total must register once")
    });

    static MCP_TOOLS_BRIDGED: LazyLock<IntGauge> = LazyLock::new(|| {
        register_int_gauge!(
            "computer_hub_mcp_tools_bridged",
            "MCP tools currently bridged into the computer hub."
        )
        .expect("computer_hub_mcp_tools_bridged must register once")
    });

    pub(crate) fn mcp_call_duration_observe(secs: f64) {
        MCP_CALL_DURATION_SECONDS.observe(secs);
    }

    pub(crate) fn mcp_error() {
        MCP_ERRORS_TOTAL.inc();
    }

    pub(crate) fn mcp_tools_bridged_set(count: i64) {
        MCP_TOOLS_BRIDGED.set(count);
    }
}

#[cfg(not(feature = "metrics"))]
mod inner {
    pub(crate) fn mcp_call_duration_observe(_secs: f64) {}
    pub(crate) fn mcp_error() {}
    pub(crate) fn mcp_tools_bridged_set(_count: i64) {}
}

pub(crate) use inner::*;
