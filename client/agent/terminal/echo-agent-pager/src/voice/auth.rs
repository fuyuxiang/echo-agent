//! Bridge the shell's `AuthManager` onto the voice crate's bearer provider.
//!
//! voice-api accepts both API keys and OAuth2 tokens directly at `api.echo-agent.invalid`
//! and attributes per-user billing for OAuth, so the voice channel just reuses
//! the same bearer the agent uses for chat — no separate env var.
//!
//! Resolved per request: the agent's refreshing manager in direct-spawn mode,
//! or a non-refreshing one that adopts the agent's rotated `auth.json` token
//! under the file lock in leader mode (see [`crate::acp`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use echo_agent_tools::types::SharedApiKeyProvider;
use echo_agent_voice::{SharedVoiceAuth, VoiceAuthProvider};

/// Adapts the shell's `ApiKeyProvider` onto [`VoiceAuthProvider`].
///
/// Resolves a token per request (never a static snapshot) so a long session
/// follows the underlying `AuthManager` instead of pinning a token that 401s.
struct AuthManagerVoiceAuth(SharedApiKeyProvider);

impl std::fmt::Debug for AuthManagerVoiceAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthManagerVoiceAuth")
    }
}

impl VoiceAuthProvider for AuthManagerVoiceAuth {
    fn bearer(&self) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        let provider = self.0.clone();
        Box::pin(async move { provider.current_api_key_async().await })
    }
}

/// Build the voice bearer provider from the connection's `AuthManager`.
///
/// Works for every auth method: OAuth / cloud.echo-agent.invalid / OIDC session tokens and
/// `ECHO_AGENT_API_KEY` / per-model BYOK keys.
pub fn build_voice_auth(
    auth_manager: Arc<echo_agent_runtime::auth::AuthManager>,
) -> SharedVoiceAuth {
    Arc::new(AuthManagerVoiceAuth(
        echo_agent_runtime::auth::shared_api_key_provider(auth_manager),
    ))
}
