//! Backpressure signal emitted by the event bus when a consumer falls
//! behind.
//!
//! `EventLag` is a wire-format payload (it
//! appears in the `EventEnvelope.payload.lag` oneof over the gRPC
//! `Events` stream), so the canonical Rust definition belongs in this
//! crate. The runtime `EventStream<T>` wrapper (in the workspace crate)
//! will surface lag to consumers as `Result<T, EventLag>`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Backpressure signal emitted when the event-bus subscriber lags
/// behind the producer and events are dropped.
///
/// Tagged with `tag = "type"` to match the global "all wire enums use
/// `tag = \"type\"`" convention. The `Lagged(u64)` variant carries the
/// number of events that were dropped between the previous successful
/// receive and the current one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum EventLag {
    /// The consumer fell behind by `n` events.
    #[error("lagged by {0} events")]
    Lagged(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let lag = EventLag::Lagged(42);
        let json = serde_json::to_string(&lag).unwrap();
        let back: EventLag = serde_json::from_str(&json).unwrap();
        assert_eq!(lag, back);
    }

    #[test]
    fn display_renders_count() {
        assert_eq!(EventLag::Lagged(7).to_string(), "lagged by 7 events");
    }

    #[test]
    fn json_shape_uses_type_tag_with_data_payload() {
        let lag = EventLag::Lagged(3);
        let json = serde_json::to_string(&lag).unwrap();
        assert_eq!(json, r#"{"type":"lagged","data":3}"#);
    }
}
