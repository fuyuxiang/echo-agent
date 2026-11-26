//! Per-turn prompt latency measurement.
//!
//! Implementation lives in `echo-agent-telemetry::prompt_timing`. This shim
//! keeps `crate::session::prompt_timing::PromptTiming` resolving at the
//! original path so callers don't need to change imports.

pub(crate) use echo_agent_telemetry::prompt_timing::PromptTiming;
