use agent_client_protocol as acp;

use crate::auth::{AuthManager, EchoAgentAuth};

/// Require EchoAgent auth from a sync context, accepting tokens in the client-side buffer window.
pub(crate) fn require_echo_agent_auth(
    auth_manager: &AuthManager,
    missing_message: &'static str,
    non_echo_agent_message: &'static str,
) -> Result<EchoAgentAuth, acp::Error> {
    let auth = auth_manager
        .current_or_expired()
        .ok_or_else(|| acp::Error::auth_required().data(missing_message))?;
    if !auth.is_echo_agent_auth() {
        return Err(acp::Error::auth_required().data(non_echo_agent_message));
    }
    Ok(auth)
}
