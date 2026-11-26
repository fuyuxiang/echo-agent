//! Response envelope for `workspace.*` methods. Wire shape:
//!
//! ```json
//! {"ok": <value>}
//! {"err": {"code": "<code>", "message": "<message>"}}
//! ```

use serde::{Deserialize, Serialize};

/// Wire code for "the target session has an active turn" rejections of
/// toolset mutations (`workspace.update_tool_config`). Retryable at the
/// turn boundary. Shared so clients can recognise the retryable class
/// without depending on the workspace crate's error enum.
pub const TURN_ACTIVE: &str = "turn_active";

pub const HUB_ERROR: &str = "hub_error";

pub const UNKNOWN_METHOD: &str = "unknown_method";

/// Legacy shape: binaries predating [`UNKNOWN_METHOD`] report it under [`HUB_ERROR`].
pub const UNKNOWN_METHOD_ERR_PREFIX: &str = "unknown workspace method:";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcEnvelope<T> {
    Ok(T),
    Err(RpcError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    /// Discriminant code (e.g. `"session_not_found"`, `"hub_error"`).
    pub code: String,
    pub message: String,
}

impl RpcError {
    /// Whether this error is a [`TURN_ACTIVE`] rejection, retryable at the
    /// turn boundary.
    pub fn is_turn_active(&self) -> bool {
        self.code == TURN_ACTIVE
    }

    /// True for every wire shape an unserved method arrives in; relays re-wrap it.
    pub fn is_unknown_method(&self) -> bool {
        self.code == UNKNOWN_METHOD || self.message.contains(UNKNOWN_METHOD_ERR_PREFIX)
    }
}

impl<T> RpcEnvelope<T> {
    pub fn into_result(self) -> Result<T, RpcError> {
        match self {
            Self::Ok(v) => Ok(v),
            Self::Err(e) => Err(e),
        }
    }

    pub fn ok(value: T) -> Self {
        Self::Ok(value)
    }

    pub fn err_parts(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Err(RpcError {
            code: code.into(),
            message: message.into(),
        })
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_wire_shape() {
        let env: RpcEnvelope<String> = RpcEnvelope::ok("hello".into());
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json, serde_json::json!({"ok": "hello"}));
    }

    #[test]
    fn err_wire_shape() {
        let env: RpcEnvelope<String> = RpcEnvelope::err_parts("session_not_found", "ghost");
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"err": {"code": "session_not_found", "message": "ghost"}})
        );
    }

    #[test]
    fn serde_round_trip_ok() {
        let env: RpcEnvelope<String> = RpcEnvelope::ok("hello".into());
        let json = serde_json::to_value(&env).unwrap();
        let recovered: RpcEnvelope<String> = serde_json::from_value(json).unwrap();
        assert_eq!(recovered.into_result().unwrap(), "hello");
    }

    #[test]
    fn serde_round_trip_err() {
        let env: RpcEnvelope<String> = RpcEnvelope::err_parts("hub_error", "boom");
        let json = serde_json::to_value(&env).unwrap();
        let recovered: RpcEnvelope<String> = serde_json::from_value(json).unwrap();
        let err = recovered.into_result().unwrap_err();
        assert_eq!(err.code, "hub_error");
        assert_eq!(err.message, "boom");
    }

    #[test]
    fn is_turn_active_matches_only_the_turn_active_code() {
        let env: RpcEnvelope<String> = RpcEnvelope::err_parts(TURN_ACTIVE, "busy");
        assert!(env.into_result().unwrap_err().is_turn_active());
        let env: RpcEnvelope<String> = RpcEnvelope::err_parts("hub_error", "boom");
        assert!(!env.into_result().unwrap_err().is_turn_active());
    }

    #[test]
    fn is_unknown_method_matches_both_wire_shapes() {
        let err = RpcError {
            code: UNKNOWN_METHOD.to_owned(),
            message: "workspace.export_github".to_owned(),
        };
        assert!(err.is_unknown_method());

        let legacy = RpcError {
            code: HUB_ERROR.to_owned(),
            message: format!("{UNKNOWN_METHOD_ERR_PREFIX} workspace.export_github"),
        };
        assert!(legacy.is_unknown_method());

        let rewrapped = RpcError {
            code: HUB_ERROR.to_owned(),
            message: format!(
                "unknown error code: {UNKNOWN_METHOD}: {UNKNOWN_METHOD_ERR_PREFIX} \
                 workspace.export_github"
            ),
        };
        assert!(rewrapped.is_unknown_method());

        let other_hub_error = RpcError {
            code: HUB_ERROR.to_owned(),
            message: "boom".to_owned(),
        };
        assert!(!other_hub_error.is_unknown_method());
    }
}
