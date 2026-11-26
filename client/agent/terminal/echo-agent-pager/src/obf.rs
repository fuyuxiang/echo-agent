//! Compile-time obfuscated string constants for binary hardening.
//!
//! Secrets that would be valuable to extract from the binary (tokens, auth
//! mechanism names, secret env var names) are encrypted at compile time via
//! [`obfstr`] and decrypted into stack-local buffers at runtime.
//!
//! Each identifier is a `macro_rules!` macro that expands at the call site,
//! so the decrypted temporary lives in the caller's scope.

/// Authentication mechanism identifiers.
pub mod auth {
    /// The internal `cached_token` auth method ID used for reconnection.
    macro_rules! CACHED_TOKEN {
        () => {
            obfstr::obfstr!("cached_token")
        };
    }
    pub(crate) use CACHED_TOKEN;
}
