//! `echo.agent/session/load_history`: fetch one older page of a gateway-backed
//! conversation by client-owned cursor (`beforeId` → `nextBeforeId`).
use super::ExtResult;
use crate::agent::MvpAgent;
use agent_client_protocol as acp;
#[tracing::instrument(skip_all, fields(method = %args.method))]
pub(crate) async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    {
        let _ = (agent, args);
        Err(acp::Error::method_not_found())
    }
}
