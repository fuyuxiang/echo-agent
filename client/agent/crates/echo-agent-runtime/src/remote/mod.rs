//! Remote storage client for the backend.

pub mod agent;
pub(crate) mod chat_models_client;
pub mod client;
pub mod conversations_client;
pub mod pull;
#[cfg(test)]
mod pull_smoke_test;
pub(crate) mod skills_client;
pub mod sync;
pub mod workspaces_client;

pub use agent::{
    SandboxClient, SandboxCreateEnvironmentRequest, SandboxEnvironment, SandboxEnvironmentResponse,
    SandboxEnvironmentVariable, SandboxEnvironmentWithMetadata, SandboxForkRequest,
    SandboxForkResponse, SandboxForkedSession, SandboxHibernateResponse,
    SandboxListEnvironmentsRequest, SandboxListEnvironmentsResponse,
    SandboxListPreinstalledPackagesResponse, SandboxLogsExitCodes, SandboxLogsResponse,
    SandboxMode, SandboxPreinstalledPackage, SandboxRestoreRequest, SandboxRestoreResponse,
    SandboxSecretInput, SandboxStartRequest, SandboxStartResponse, SandboxStatusResponse,
    SandboxTerminateRequest, SandboxUpdateEnvironmentRequest,
};
pub use chat_models_client::{
    ChatModelsClient, ChatModelsError, ListModesResponse, Mode, ModeAvailability,
};
pub use client::{
    BackendClient, BackendError, FetchModelsResult, FetchedBundle, SettingsFetch, fetch_bundle,
    fetch_login_device_flow, fetch_settings_blocking, fetch_subagent_bundle, share_url,
};
pub(crate) use client::{DEFAULT_CONTEXT_WINDOW, fetch_models_blocking, models_list_url};
pub use conversations_client::{
    ConvError, ConvQuery, Conversation, ConversationsClient, ListConversationsPage,
    UpdateConversationBody,
};
pub use pull::{PullResult, pull_session_to_local};
pub use skills_client::{
    BundledSkill, CHAT_PRODUCT_META_KEY, CHAT_PRODUCT_META_VALUE, ListBundledSkillsResponse,
    ListUserSkillsResponse, ProductSkillsCatalog, SkillsClient, SkillsError, UserSkill,
};
pub use sync::RemoteSync;
pub use workspaces_client::{ListWorkspacesPage, Workspace, WorkspacesClient, WsError, WsQuery};
