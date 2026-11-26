//! Macros for printing messages with Unix timestamps.
//!
//! These macros use tracing for output, which is safe to use in headless mode
//! (no terminal attached). The timestamp is included in the message.

/// Prints a message via tracing::info with a Unix timestamp prefix.
///
/// The format is: `{unix_timestamp}::{message}`
///
/// # Examples
///
/// ```ignore
/// tprintln!("Hello, world!");
/// // Logs: 1234567890::Hello, world!
///
/// tprintln!("Value: {}", 42);
/// // Logs: 1234567890::Value: 42
/// ```
#[macro_export]
macro_rules! tprintln {
    () => {{
        let ts = ::std::time::SystemTime::now()
            .duration_since(::std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ::tracing::info!("{}::", ts)
    }};
    ($($arg:tt)*) => {{
        let ts = ::std::time::SystemTime::now()
            .duration_since(::std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ::tracing::info!("{}::{}", ts, ::std::format_args!($($arg)*))
    }};
}

/// Prints a message via tracing::warn with a Unix timestamp prefix.
///
/// The format is: `{unix_timestamp}::{message}`
///
/// # Examples
///
/// ```ignore
/// teprintln!("Error occurred!");
/// // Logs: 1234567890::Error occurred!
///
/// teprintln!("Error code: {}", 500);
/// // Logs: 1234567890::Error code: 500
/// ```
#[macro_export]
macro_rules! teprintln {
    () => {{
        let ts = ::std::time::SystemTime::now()
            .duration_since(::std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ::tracing::warn!("{}::", ts)
    }};
    ($($arg:tt)*) => {{
        let ts = ::std::time::SystemTime::now()
            .duration_since(::std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ::tracing::warn!("{}::{}", ts, ::std::format_args!($($arg)*))
    }};
}
