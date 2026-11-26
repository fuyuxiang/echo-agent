//! EchoAgent Computer Hub — transport + registry + resolver core.
//!
//! Object-safe abstractions used by every router build: a [`Transport`]
//! that authorises and dispatches calls, a [`ToolRegistry`] trait shared
//! by both storage planes, a [`CompoundResolver`] that applies the
//! local-shadows-remote rule, and the local + remote transports plus
//! inner-dispatch glue that sit on top.

#![forbid(unsafe_code)]

pub mod inner;
pub mod local;
pub mod registry;
pub mod remote;
pub mod resolver;
pub mod transport;

pub use inner::InnerDispatchForResolver;
pub use local::{LOCAL_INVOKE_SCOPE, LocalTransport};
pub use registry::{
    ConnectionCleanupReport, SessionCleanupReport, ToolRegistry, ToolSessionBindOutcome,
    ToolSessionUnbindOutcome,
};
pub use remote::{
    ConnectionClient, RemoteToolProxy, RemoteTransport, decode_call_result, error_from_envelope,
    is_workspace_unavailable, output_to_value, progress_from_frame, tool_error_from_wire,
};
pub use resolver::{CompoundResolver, ErasedTool, ResolvedTool, ToolHandle};
pub use transport::{Principal, Transport, TransportKind};
