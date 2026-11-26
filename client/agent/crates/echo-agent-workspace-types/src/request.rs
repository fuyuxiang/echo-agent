//! The wire-side request envelope.
//!
//! # Why `RequestMessage<T>` and not a `Request<T>` with runtime fields?
//!
//! A `Request<T>` with `cancel:
//! CancellationToken` and `extensions: Extensions` fields is tempting. Both are
//! **runtime concerns**, not wire concerns:
//!
//! - `tokio_util::sync::CancellationToken` is a tokio type. Adding it
//!   here would force every consumer of `echo-agent-workspace-types`
//!   (including the eventual WASM browser SDK) to pull in tokio, which
//!   defeats the whole point of having a separate wire-types crate.
//!   Cancellation is a transport mechanism: in-process, the receiver
//!   drop signal handles it; over gRPC, the client closing the stream
//!   handles it.
//! - `Extensions` (a typed `HashMap<TypeId, Box<dyn Any + Send + Sync>>`)
//!   is explicitly "in-process only; not serialized" per the doc. It
//!   carries tracing spans and telemetry context that have no wire
//!   representation -- they belong with the runtime.
//!
//! So this crate exposes `RequestMessage<T>`: just the parts that need
//! to survive a network hop ([`message`](RequestMessage::message),
//! [`metadata`](RequestMessage::metadata), and an optional
//! [`deadline`](RequestMessage::deadline)). The runtime crate
//! (`echo-agent-workspace`) wraps this in its own `Request<T>` that
//! adds the cancellation token and extensions map.
//!
//! Splitting the envelope this way keeps `echo-agent-workspace-types`
//! tokio-free while preserving a clean lift from wire to runtime types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::metadata::Metadata;

/// Wire-side request envelope.
///
/// Wraps a typed payload ([`message`](RequestMessage::message)) with
/// per-call [`metadata`](RequestMessage::metadata) and an optional
/// [`deadline`](RequestMessage::deadline). The runtime envelope adds
/// cancellation and an in-process extensions map -- see this module's
/// doc comment for why those fields live there and not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestMessage<T> {
    /// The typed request payload (one of the `*Request` enums).
    pub message: T,

    /// String-keyed metadata for the call (auth tokens, trace context,
    /// session id, ...). See [`crate::metadata`] for the standard keys.
    #[serde(default)]
    pub metadata: Metadata,

    /// Optional absolute deadline for the call, in UTC.
    ///
    /// Encoded as an ISO-8601 string in JSON. The runtime layer is
    /// responsible for translating this to a tokio sleep / gRPC
    /// `grpc-timeout` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<DateTime<Utc>>,
}

impl<T> RequestMessage<T> {
    /// Construct a new request with empty metadata and no deadline.
    pub fn new(message: T) -> Self {
        Self {
            message,
            metadata: Metadata::default(),
            deadline: None,
        }
    }

    /// Builder: attach metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Builder: set the absolute deadline.
    #[must_use]
    pub fn with_deadline(mut self, deadline: DateTime<Utc>) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Map the inner payload while preserving metadata and deadline.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> RequestMessage<U> {
        RequestMessage {
            message: f(self.message),
            metadata: self.metadata,
            deadline: self.deadline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::META_SESSION_ID;

    #[test]
    fn round_trips_with_string_payload() {
        let mut meta = Metadata::default();
        meta.insert(META_SESSION_ID, "s1");
        let req = RequestMessage::new("hello".to_string()).with_metadata(meta);
        let json = serde_json::to_string(&req).unwrap();
        let back: RequestMessage<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn omits_deadline_when_none() {
        let req = RequestMessage::new(42_u32);
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("deadline"), "got {json}");
    }

    #[test]
    fn round_trips_with_deadline() {
        let when = chrono::Utc::now();
        let req = RequestMessage::new(42_u32).with_deadline(when);
        let json = serde_json::to_string(&req).unwrap();
        let back: RequestMessage<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn map_preserves_metadata_and_deadline() {
        let when = chrono::Utc::now();
        let mut meta = Metadata::default();
        meta.insert("k", "v");
        let req = RequestMessage::new(1_u32)
            .with_metadata(meta.clone())
            .with_deadline(when);
        let mapped = req.map(|n| n.to_string());
        assert_eq!(mapped.message, "1");
        assert_eq!(mapped.metadata, meta);
        assert_eq!(mapped.deadline, Some(when));
    }
}
