//! Bearer resolution for voice STT requests.
//! The voice clients are long-lived: a single voice session opens many STT
//! WebSocket connections over its lifetime, and an OAuth/session bearer rotates
//! (~15 min). Capturing a token once at startup would 401 mid-session. So
//! instead of a static `String`, the clients hold a [`SharedVoiceAuth`] and
//! resolve a fresh bearer at the point of each connection.
//!
//! This crate stays dependency-light: it defines its own minimal async trait
//! rather than depending on the shell's `AuthManager` / tools' `ApiKeyProvider`.
//! The pager adapts the shell's refreshing provider onto this trait.

use std::future::{Future, ready};
use std::pin::Pin;
use std::sync::Arc;

#[cfg(feature = "audio")]
use crate::error::VoiceError;

pub trait VoiceAuthProvider: std::fmt::Debug + Send + Sync + 'static {
    fn bearer(&self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
}

/// Shared provider handed to the voice pipeline.
pub type SharedVoiceAuth = Arc<dyn VoiceAuthProvider>;

#[cfg(feature = "audio")]
pub(crate) async fn require_bearer(auth: &SharedVoiceAuth) -> Result<String, VoiceError> {
    auth.bearer().await.ok_or_else(|| {
        VoiceError::Auth(
            "not signed in — run `echoagent login`, set ECHO_AGENT_API_KEY, or set a model api_key/env_key"
                .into(),
        )
    })
}

/// A fixed bearer that never refreshes.
///
/// Used by the standalone `voice-probe` binary and tests, where there is no
/// `AuthManager` — only a raw `ECHO_AGENT_API_KEY`.
pub struct StaticVoiceAuth(pub String);

impl std::fmt::Debug for StaticVoiceAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("StaticVoiceAuth")
            .field(&"<redacted>")
            .finish()
    }
}

impl VoiceAuthProvider for StaticVoiceAuth {
    fn bearer(&self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(ready(Some(self.0.clone())))
    }
}

impl StaticVoiceAuth {
    /// Build a [`SharedVoiceAuth`] from a static key, trimming whitespace and
    /// rejecting an empty value.
    pub fn shared(key: impl Into<String>) -> Option<SharedVoiceAuth> {
        let key = key.into().trim().to_string();
        if key.is_empty() {
            return None;
        }
        Some(Arc::new(Self(key)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_provider_resolves() {
        let provider = StaticVoiceAuth::shared("  sk-test  ").unwrap();
        assert_eq!(provider.bearer().await.as_deref(), Some("sk-test"));
    }

    #[test]
    fn static_provider_rejects_empty() {
        assert!(StaticVoiceAuth::shared("   ").is_none());
    }
}
