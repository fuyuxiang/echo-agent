//! OIDC authentication: protocol, login, and refresh submodules.

mod login;
pub(crate) mod protocol;
pub(crate) mod refresh;
#[cfg(test)]
mod test_helpers;

pub use login::{run_login_flow, run_login_flow_with_config};
pub(crate) use protocol::{
    enforce_login_principal, is_configured, login_principal_policy, peek_access_token_principal,
    peek_access_token_principal_id, with_alpha_test_key,
};
pub(crate) use refresh::{OidcRefreshResult, oidc_token_exchange};
