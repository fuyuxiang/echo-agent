//! Host-agnostic agent lifecycle hooks shared by multiple agent hosts (e.g. echo-agent-runtime).
//! Contributors receive data-only per-hook inputs at dispatch time; anything they act through is a
//! capability injected at install time, and they never own loop control.

pub mod local;
pub mod send;

pub use local::{
    LocalCommandContributor, LocalExtensionRegistry, LocalExtensionRegistryBuilder,
    LocalSessionLifecycleContributor, LocalTurnInputContributor, LocalTurnLifecycleContributor,
};
pub use send::{
    AnalyticsClass, CommandAction, CommandContributor, CommandInvocation, CommandSpec,
    CompactionClass, ExtensionRegistry, ExtensionRegistryBuilder, InputAuthority, InputPolicy,
    QueuePolicy, SessionIdleInput, SessionLifecycleContributor, ShutdownPolicy, TurnAbortInput,
    TurnAbortReason, TurnBoundary, TurnDoneInput, TurnErrorInput, TurnInputContext,
    TurnInputContributor, TurnInputFragment, TurnLifecycleContributor, TurnStartInput,
};
