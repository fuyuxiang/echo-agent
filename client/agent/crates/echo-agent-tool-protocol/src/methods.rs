//! Closed enumeration of every JSON-RPC method on the wire.
//!
//! Each variant is defined once in the [`define_methods!`] macro invocation
//! together with its wire string. The macro generates the enum, serde
//! renames, [`Method::as_wire_str`], and [`Method::from_wire_str`] from
//! that single source of truth.

use serde::{Deserialize, Serialize};

macro_rules! define_methods {
    (
        $(
            $(#[$var_attr:meta])*
            $variant:ident => $wire:literal
        ),* $(,)?
    ) => {
        /// Every JSON-RPC method understood by the computer hub.
        ///
        /// The variants are grouped by direction in source order; the enum is
        /// flat — direction enforcement is the computer hub's job, not the
        /// protocol crate's.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Method {
            $(
                $(#[$var_attr])*
                #[serde(rename = $wire)]
                $variant,
            )*
        }

        impl Method {
            /// Every `Method` variant, for exhaustive iteration in tests.
            pub const ALL: &'static [Method] = &[$(Self::$variant,)*];

            /// Wire string for this method. Equivalent to the serde
            /// serialization but without a round-trip through `serde_json`.
            pub const fn as_wire_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)*
                }
            }

            /// Inverse of [`Self::as_wire_str`]. Returns `None` for
            /// strings that don't match any known method.
            pub fn from_wire_str(s: &str) -> Option<Self> {
                match s {
                    $($wire => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

/// Message prefix the hub uses when rejecting a request whose `method`
/// string does not parse into [`Method`] — the shape an OLD hub produces
/// for verbs it predates. Current clients answer hub skew from the
/// `hello_ack` `capabilities` advertisement instead of sniffing this
/// message, but the shape stays pinned here: terminal binaries built
/// while the SDK still keyed old-hub detection on this exact prefix
/// remain in the fleet. Do not change casually.
pub const UNKNOWN_METHOD_MSG_PREFIX: &str = "unknown method `";

define_methods! {
    // harness → service
    SessionOpen => "session_open",
    SessionClose => "session_close",
    SessionBindServer => "session_bind_server",
    SessionUnbindServer => "session_unbind_server",
    /// Attach this harness connection to an EXISTING session as an
    /// observer. Answered hub-locally from the session→tool-server
    /// routing established by the owner's `session_bind_server` (or the
    /// server's re-`serve`); never forwarded to the tool server.
    SessionAttachServer => "session_attach_server",
    ToolsList => "tools.list",
    ToolsSearch => "tools.search",
    ToolCall => "tool.call",
    /// Sugar for [`Method::Hook`] with [`crate::HookEvent::Cancel`].
    /// SDKs translate this method to a hook frame before sending; there
    /// is no separate `tool.cancel` wire frame and no `ToolCancelParams`
    /// struct in [`crate::frames`].
    ToolCancel => "tool.cancel",
    ToolNotify => "tool.notify",
    SystemNotify => "system.notify",
    SubscribeNotifications => "subscribe_notifications",
    UnsubscribeNotifications => "unsubscribe_notifications",
    Hook => "hook",
    Hello => "hello",
    HelloAck => "hello_ack",
    Ping => "ping",
    Pong => "pong",

    // tool_server → service
    ToolCallProgress => "tool_call_progress",
    ToolNotification => "tool.notification",
    /// Reply to a request/response hook, correlated back to the harness by `hook_id`.
    HookReply => "hook_reply",
    /// Notification (no `id`, no response); rejects surface only in
    /// hub metrics. Only hub-minted trace-ids are accepted.
    TracesDonate => "traces.donate",
    /// Notification (no `id`, no response); rejects surface only in hub
    /// metrics. Donor service.name must be hub-allowlisted.
    LogsDonate => "logs.donate",
    /// Notification (no `id`, no response); rejects surface only in hub
    /// metrics. Donor service.name must be hub-allowlisted. No envelope
    /// `session_id` — metrics are process-aggregate.
    MetricsDonate => "metrics.donate",

    // service → tool_server
    ToolCallRequest => "tool_call_request",

    // service → harness
    ToolsChanged => "tools_changed",
    SubscribeAck => "subscribe_ack",
    UnsubscribeAck => "unsubscribe_ack",

    // harness → service (server discovery)
    /// List available tool servers for the authenticated user.
    ServersList => "servers.list",

    // tool_server status lifecycle
    ToolServerStatus => "tool_server.status",
    ToolServerGetStatus => "tool_server.get_status",
    ToolServerEvict => "tool_server.evict",

    // ── Session lifecycle ───────────────────────────────────────────

    /// Full tool snapshot for a session (server → hub). Idempotent:
    /// re-sending replaces the tool set; the hub diffs and emits
    /// `tools_changed`.
    Serve => "serve",
    /// Hub requests the server to start serving a session
    /// (hub → server). The server responds with its tool snapshot.
    SessionBind => "session.bind",
    /// Hub tells the server to stop serving a session
    /// (hub → server). Notification — no response expected.
    SessionUnbind => "session.unbind",
}

impl std::fmt::Display for Method {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_as_wire_str_from_wire_str() {
        let all = [
            Method::SessionOpen,
            Method::SessionClose,
            Method::SessionBindServer,
            Method::SessionUnbindServer,
            Method::SessionAttachServer,
            Method::ToolsList,
            Method::ToolsSearch,
            Method::ToolCall,
            Method::ToolCancel,
            Method::ToolNotify,
            Method::SystemNotify,
            Method::SubscribeNotifications,
            Method::UnsubscribeNotifications,
            Method::Hook,
            Method::Hello,
            Method::HelloAck,
            Method::Ping,
            Method::Pong,
            Method::ToolCallProgress,
            Method::ToolNotification,
            Method::HookReply,
            Method::TracesDonate,
            Method::LogsDonate,
            Method::MetricsDonate,
            Method::ToolCallRequest,
            Method::ToolsChanged,
            Method::SubscribeAck,
            Method::UnsubscribeAck,
            Method::ServersList,
            Method::ToolServerStatus,
            Method::ToolServerGetStatus,
            Method::ToolServerEvict,
            Method::Serve,
            Method::SessionBind,
            Method::SessionUnbind,
        ];
        for m in all {
            assert_eq!(Method::from_wire_str(m.as_wire_str()), Some(m));
        }
    }

    #[test]
    fn from_wire_str_returns_none_for_unknown() {
        assert_eq!(Method::from_wire_str("not_a_method"), None);
        assert_eq!(Method::from_wire_str(""), None);
    }

    #[test]
    fn serde_round_trip_matches_wire_str() {
        let method = Method::ToolCall;
        let json = serde_json::to_value(method).expect("serialize");
        assert_eq!(json.as_str(), Some("tool.call"));
        let back: Method = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, method);
    }
}
