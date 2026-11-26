//! Shim — see `echo_agent_telemetry::instrumentation` for the implementation.
//!
//! Two pieces stay here:
//! - The [`instrumentation_timer!`] macro, because it's `#[macro_export]`-ed
//!   from this crate and call sites spell it `crate::instrumentation_timer!`
//!   (i.e. `echo_agent_runtime::instrumentation_timer!`). Keeping the macro here
//!   means downstream callers don't need to be edited.
//! - [`finalize_and_exit`], because shell needs to log a terminal exit event
//!   and shut down the shared OTel pipeline before the process exits. The
//!   telemetry crate exposes the shutdown helper, so this thin wrapper just
//!   plumbs it together with `process::exit`.

pub use echo_agent_telemetry::instrumentation::{
    ChromeTraceOptions, InstrumentationFinalizer, InstrumentationMode, InstrumentationTimer,
    TARGET, current_mode, finalize, finalizer, generate_chrome_trace, install_panic_hook, layer,
    timer,
};

/// Final cleanup before terminating the process.
///
/// Logs an exit event, flushes instrumentation guards, shuts down the
/// OpenTelemetry pipeline, and exits with `code`.
///
/// Stays in shell so callers can keep calling `echo_agent_runtime::instrumentation::finalize_and_exit`.
pub fn finalize_and_exit(code: i32) -> ! {
    let signal_name = match code {
        130 => "SIGINT",
        143 => "SIGTERM",
        _ => "other",
    };
    tracing::info!(
        event_type = "process_exit",
        signal = signal_name,
        exit_code = code,
        "Exiting process"
    );
    let _ = finalize();
    echo_agent_telemetry::otel_layer::shutdown_otel();
    // Flush the --debug firehose; this exits via process::exit, bypassing main's flush.
    echo_agent_telemetry::debug_log::flush();
    std::process::exit(code);
}

/// Time a block under the instrumentation target.
///
/// Macro stays in shell so `$crate` continues to resolve to `echo_agent_runtime`
/// for the 12+ existing call sites that spell it as
/// `crate::instrumentation_timer!(...)` or `echo_agent_runtime::instrumentation_timer!(...)`.
/// The macro body delegates to types and functions in
/// `echo_agent_telemetry::instrumentation`.
#[macro_export]
macro_rules! instrumentation_timer {
    ($name:literal) => {{
        let mode = $crate::instrumentation::current_mode();
        match mode {
            $crate::instrumentation::InstrumentationMode::Chrome => {
                let span = tracing::info_span!(target: $crate::instrumentation::TARGET, $name);
                $crate::instrumentation::InstrumentationTimer::new_with_span(
                    $name,
                    mode,
                    Some(span.entered()),
                )
            }
            _ => $crate::instrumentation::InstrumentationTimer::new($name),
        }
    }};
}
