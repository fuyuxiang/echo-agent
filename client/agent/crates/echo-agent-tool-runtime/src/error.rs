//! Cross-ecosystem error type for tool execution.
//!
//! `ToolError` is a struct with a `kind` discriminator and a tool-provided
//! `detail` string. The `detail` is the model-facing message — tools MUST
//! provide a human-readable explanation of what went wrong, since this text
//! is sent back to the model to inform its next action.
//!
//! The wire boundary is bridged by `From<ToolError> for ToolErrorWire`.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use echo_agent_tool_protocol::{ToolErrorWire, ToolId};

/// Discriminator for tool errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolErrorKind {
    /// The tool has no implementation for the requested operation.
    NotImplemented,
    /// Inputs failed validation.
    InvalidArguments,
    /// No tool registered under the given id.
    NotFound,
    /// Caller lacks required permissions (403-shaped).
    PermissionDenied,
    /// Authentication failed (401-shaped).
    Unauthorized,
    /// The tool ran past its time budget.
    Timeout,
    /// The caller cancelled the tool call.
    Cancelled,
    /// Rate limit exceeded.
    RateLimited,
    /// The caller's usage pool / billing balance is exhausted (out
    /// of credits). Payment-required-shaped; distinct from
    /// `RateLimited` so the surface can show "out of credits"
    /// rather than "try again later".
    UsagePoolExhausted,
    /// The caller hit a usage limit with no balance verdict behind it
    /// (the balance gate was skipped/dormant and the non-billable
    /// allowance ran out). Payment-required-shaped, but distinct from
    /// `UsagePoolExhausted` (an explicit out-of-balance verdict) so the
    /// surface can show a "usage limit reached" message.
    UsageLimitReached,
    /// A shared-capacity limiter shed this request (transient load
    /// shed): the billing global rate limiter, or the sandbox fleet's
    /// tenancy cap. Distinct from `RateLimited` (per-user / per-message
    /// quota) so the surface can render a capacity-specific
    /// "try again later" with a retry hint; the `retry_after_secs`
    /// hint, when known, rides in `ToolError::details`. Named to match
    /// the chat surface's `global_rate_limit` typed error.
    GlobalRateLimit,
    /// The caller hit their per-user concurrency cap (too many media
    /// generations or sandboxes already in flight). Transient — retry
    /// once one finishes. Distinct from `GlobalRateLimit` (a
    /// shared-backend load shed) so the surface can tailor a "too many
    /// in progress" message. Named to match the chat surface's
    /// `concurrency_limit` typed error.
    ConcurrencyLimit,
    /// Upstream service unavailable.
    ServiceUnavailable,
    /// Network-level failure.
    NetworkError,
    /// Tool body returned an error.
    Execution,
    /// Requested behavior version not supported.
    BehaviorVersionUnsupported,
    /// Render-card budget exceeded.
    RenderLimited,
    /// Terminal subprocess failure.
    TerminalError,
    /// Forward-compat catch-all.
    Custom,
}

impl ToolErrorKind {
    /// Snake-case identifier for metrics / logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotImplemented => "not_implemented",
            Self::InvalidArguments => "invalid_arguments",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::Unauthorized => "unauthorized",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::RateLimited => "rate_limited",
            Self::UsagePoolExhausted => "usage_pool_exhausted",
            Self::UsageLimitReached => "usage_limit_reached",
            Self::GlobalRateLimit => "global_rate_limit",
            Self::ConcurrencyLimit => "concurrency_limit",
            Self::ServiceUnavailable => "service_unavailable",
            Self::NetworkError => "network_error",
            Self::Execution => "execution",
            Self::BehaviorVersionUnsupported => "behavior_version_unsupported",
            Self::RenderLimited => "render_limited",
            Self::TerminalError => "terminal_error",
            Self::Custom => "custom",
        }
    }
}

/// Cross-ecosystem error type for tool execution.
///
/// Every error carries:
/// - `kind` — the machine-readable discriminator
/// - `detail` — the model/user-facing message that tools MUST provide
/// - `source` — optional causal chain for debugging (not sent to the model)
/// - `details` — optional structured metadata (JSON Schema validation
///   report, retry_after hints, etc.)
#[derive(Serialize, Deserialize)]
pub struct ToolError {
    pub kind: ToolErrorKind,
    /// Human-readable message provided by the tool. This is sent back to
    /// the model so it can understand what went wrong and adjust its next
    /// action. Tools MUST make this specific and actionable.
    pub detail: String,
    /// Optional causal chain for developer debugging. NOT sent to the model.
    #[serde(skip)]
    source: Option<anyhow::Error>,
    /// Optional structured metadata (e.g. per-field validation errors,
    /// `retry_after` hints, `tool_id`, `card_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl fmt::Debug for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("ToolError");
        d.field("kind", &self.kind);
        d.field("detail", &self.detail);
        if let Some(ref source) = self.source {
            d.field("source", &format!("{source:#}"));
        }
        if let Some(ref details) = self.details {
            d.field("details", details);
        }
        d.finish()
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref() as &_)
    }
}

// ---------------------------------------------------------------------------
// Constructors — one per kind for ergonomic tool code
// ---------------------------------------------------------------------------

impl ToolError {
    /// Core constructor. All other constructors delegate here.
    pub fn new(kind: ToolErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
            details: None,
        }
    }

    /// Attach structured metadata.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Attach a causal error chain (for developer logs, not sent to model).
    pub fn with_source(mut self, source: impl Into<anyhow::Error>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn not_implemented(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::NotImplemented, detail)
    }

    pub fn invalid_arguments(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::InvalidArguments, detail)
    }

    pub fn not_found(tool_id: ToolId, detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::NotFound, detail)
            .with_details(serde_json::json!({ "tool_id": tool_id.as_str() }))
    }

    pub fn permission_denied(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::PermissionDenied, detail)
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Unauthorized, detail)
    }

    pub fn timeout(tool_id: ToolId, detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Timeout, detail)
            .with_details(serde_json::json!({ "tool_id": tool_id.as_str() }))
    }

    pub fn cancelled(tool_id: ToolId, detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Cancelled, detail)
            .with_details(serde_json::json!({ "tool_id": tool_id.as_str() }))
    }

    pub fn rate_limited(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::RateLimited, detail)
    }

    pub fn usage_pool_exhausted(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::UsagePoolExhausted, detail)
    }

    pub fn usage_limit_reached(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::UsageLimitReached, detail)
    }

    pub fn global_rate_limit(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::GlobalRateLimit, detail)
    }

    pub fn concurrency_limit(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::ConcurrencyLimit, detail)
    }

    pub fn service_unavailable(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::ServiceUnavailable, detail)
    }

    pub fn network_error(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::NetworkError, detail)
    }

    pub fn execution(tool_id: ToolId, detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Execution, detail)
            .with_details(serde_json::json!({ "tool_id": tool_id.as_str() }))
    }

    pub fn terminal_error(tool_id: ToolId, detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::TerminalError, detail)
            .with_details(serde_json::json!({ "tool_id": tool_id.as_str() }))
    }

    pub fn custom(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Custom, detail)
            .with_details(serde_json::json!({ "code": code.into() }))
    }

    /// Snake-case identifier for the kind. Delegates to
    /// [`ToolErrorKind::as_str`].
    pub fn variant_name(&self) -> &'static str {
        self.kind.as_str()
    }
}

// ---------------------------------------------------------------------------
// From impls
// ---------------------------------------------------------------------------

impl From<serde_json::Error> for ToolError {
    fn from(value: serde_json::Error) -> Self {
        Self::invalid_arguments(value.to_string())
    }
}

// ---------------------------------------------------------------------------
// Wire bridge
// ---------------------------------------------------------------------------

/// Carry a [`ToolError`]'s structured `details` onto a `Custom` wire variant
/// while keeping the round-trip recognizable: the decoder
/// (`tool_error_from_wire`) replaces the `{"code": <subcode>}` object that
/// `ToolError::custom` installs with the wire `details` verbatim, so the
/// subcode is merged into object-shaped details (without clobbering an
/// existing `code` key). Non-object details pass through unchanged.
fn custom_details_with_code(details: Option<Value>, code: &str) -> Option<Value> {
    match details {
        Some(Value::Object(mut map)) => {
            map.entry("code")
                .or_insert_with(|| Value::String(code.to_owned()));
            Some(Value::Object(map))
        }
        other => other,
    }
}

impl From<ToolError> for ToolErrorWire {
    fn from(err: ToolError) -> Self {
        // Extract structured fields from `details` when the wire shape needs
        // them. The `detail` string is always the model-facing message.
        let details_val = err.details.as_ref();

        match err.kind {
            ToolErrorKind::NotImplemented => Self::Custom {
                subcode: "not_implemented".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "not_implemented"),
            },
            ToolErrorKind::InvalidArguments => Self::InvalidArguments {
                message: err.detail,
                details: err.details,
            },
            ToolErrorKind::NotFound => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                Self::ToolNotFound { tool_id }
            }
            ToolErrorKind::PermissionDenied => Self::PermissionDenied { reason: err.detail },
            ToolErrorKind::Unauthorized => Self::Custom {
                subcode: "unauthorized".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "unauthorized"),
            },
            ToolErrorKind::Timeout => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                let elapsed_ms = details_val
                    .and_then(|d| d.get("elapsed_ms"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                Self::Timeout {
                    tool_id,
                    elapsed_ms,
                }
            }
            ToolErrorKind::Cancelled => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                Self::Cancelled { tool_id }
            }
            ToolErrorKind::RateLimited => Self::Custom {
                subcode: "rate_limited".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "rate_limited"),
            },
            ToolErrorKind::UsagePoolExhausted => Self::Custom {
                subcode: "usage_pool_exhausted".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "usage_pool_exhausted"),
            },
            ToolErrorKind::UsageLimitReached => Self::Custom {
                subcode: "usage_limit_reached".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "usage_limit_reached"),
            },
            ToolErrorKind::GlobalRateLimit => Self::Custom {
                subcode: "global_rate_limit".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "global_rate_limit"),
            },
            ToolErrorKind::ConcurrencyLimit => Self::Custom {
                subcode: "concurrency_limit".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "concurrency_limit"),
            },
            ToolErrorKind::ServiceUnavailable => Self::Custom {
                subcode: "service_unavailable".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "service_unavailable"),
            },
            ToolErrorKind::NetworkError => Self::Custom {
                subcode: "network_error".to_owned(),
                message: err.detail,
                details: custom_details_with_code(err.details, "network_error"),
            },
            ToolErrorKind::Execution => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                Self::Execution {
                    tool_id,
                    message: err.detail,
                }
            }
            ToolErrorKind::BehaviorVersionUnsupported => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                let requested = details_val
                    .and_then(|d| d.get("requested"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_owned();
                Self::BehaviorVersionUnsupported { tool_id, requested }
            }
            ToolErrorKind::RenderLimited => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                let card_id = details_val
                    .and_then(|d| d.get("card_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                Self::RenderLimited {
                    tool_id,
                    card_id,
                    reason: err.detail,
                }
            }
            ToolErrorKind::TerminalError => {
                let tool_id = details_val
                    .and_then(|d| d.get("tool_id"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| ToolId::new(s).ok())
                    .unwrap_or_else(|| ToolId::new("unknown").unwrap());
                Self::TerminalError {
                    tool_id,
                    message: err.detail,
                }
            }
            ToolErrorKind::Custom => {
                let subcode = details_val
                    .and_then(|d| d.get("code"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("custom")
                    .to_owned();
                Self::Custom {
                    subcode,
                    message: err.detail,
                    details: err.details,
                }
            }
        }
    }
}

#[cfg(test)]
mod wire_bridge_tests {
    use super::*;

    #[test]
    fn service_unavailable_details_survive_wire_projection() {
        // Structured details used to be dropped (`details: None`) for the
        // Custom-mapped kinds; they must now ride the wire with the subcode
        // merged in so recognizers keying on `details.code` keep working.
        let err = ToolError::service_unavailable("sandbox not ready")
            .with_details(serde_json::json!({ "retry_after_ms": 1500 }));
        let wire = ToolErrorWire::from(err);
        let ToolErrorWire::Custom {
            subcode,
            message,
            details,
        } = wire
        else {
            panic!("expected Custom");
        };
        assert_eq!(subcode, "service_unavailable");
        assert_eq!(message, "sandbox not ready");
        let details = details.expect("details preserved");
        assert_eq!(
            details.get("retry_after_ms").and_then(|v| v.as_u64()),
            Some(1500)
        );
        assert_eq!(
            details.get("code").and_then(|v| v.as_str()),
            Some("service_unavailable"),
            "subcode merged into details for round-trip recognizability",
        );
    }

    #[test]
    fn rate_limit_and_usage_kinds_merge_subcode_uniformly() {
        // Same property as service_unavailable, applied to every
        // Custom-mapped kind: object details without a `code` key gain the
        // subcode, so decode-side recognizers keying on `details.code` can
        // still classify the error.
        let cases: [(ToolError, &str); 5] = [
            (ToolError::rate_limited("slow down"), "rate_limited"),
            (
                ToolError::usage_pool_exhausted("pool empty"),
                "usage_pool_exhausted",
            ),
            (
                ToolError::usage_limit_reached("limit hit"),
                "usage_limit_reached",
            ),
            (
                ToolError::global_rate_limit("global limit"),
                "global_rate_limit",
            ),
            (
                ToolError::concurrency_limit("too many in flight"),
                "concurrency_limit",
            ),
        ];
        for (err, subcode) in cases {
            let err = err.with_details(serde_json::json!({ "retry_after_ms": 250 }));
            let ToolErrorWire::Custom {
                subcode: got,
                details,
                ..
            } = ToolErrorWire::from(err)
            else {
                panic!("expected Custom for {subcode}");
            };
            assert_eq!(got, subcode);
            let details = details.expect("details preserved");
            assert_eq!(
                details.get("code").and_then(|v| v.as_str()),
                Some(subcode),
                "subcode merged for {subcode}",
            );
            assert_eq!(
                details.get("retry_after_ms").and_then(|v| v.as_u64()),
                Some(250),
            );
        }
    }

    #[test]
    fn custom_details_with_code_does_not_clobber_existing_code() {
        let merged = custom_details_with_code(
            Some(serde_json::json!({ "code": "workspace_unavailable", "retryable": true })),
            "service_unavailable",
        )
        .expect("details kept");
        assert_eq!(
            merged.get("code").and_then(|v| v.as_str()),
            Some("workspace_unavailable"),
            "an existing code key must win",
        );
    }

    #[test]
    fn custom_details_with_code_passes_none_and_non_objects_through() {
        assert_eq!(custom_details_with_code(None, "network_error"), None);
        let arr = serde_json::json!([1, 2, 3]);
        assert_eq!(
            custom_details_with_code(Some(arr.clone()), "network_error"),
            Some(arr),
        );
    }
}
