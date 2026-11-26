use std::{borrow::Borrow, fmt, ops::Deref};

use agent_client_protocol as acp;
use derive_more::From;
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use tokio::sync::oneshot;

use crate::common::AcpResult;

pub use self::{
    agent::{AcpAgentMessage, AcpAgentMessageBox, AcpAgentMessageGeneric},
    client::{AcpClientMessage, AcpClientMessageBox, AcpClientMessageGeneric},
};

/// Marker trait representing one side of the ACP connection.
pub trait AcpSide {
    /// What does this side receive.
    type InMessage: AcpMethod + fmt::Debug;
    /// What does this side send.
    type OutMessage: AcpMethod + fmt::Debug;
    /// Marker type for the other side.
    type OtherSide: AcpSide;
    /// Display name for this side.
    const NAME: &'static str;
}

/// Marker type representing the agent's view of the ACP connection (as one side of that connection).
impl AcpSide for acp::AgentSide {
    type InMessage = AcpAgentMessage; // inbound messages = messages meant *for* the agent
    type OutMessage = AcpClientMessage; // outbound messages = messages meant *for* the client
    type OtherSide = acp::ClientSide;
    const NAME: &'static str = "agent";
}

/// Marker type representing the agent's view of the ACP connection (as one side of that connection).
impl AcpSide for acp::ClientSide {
    type InMessage = AcpClientMessage; // inbound messages = messages meant *for* the client
    type OutMessage = AcpAgentMessage; // outbound messages = messages meant *for* the agent
    type OtherSide = acp::AgentSide;
    const NAME: &'static str = "client";
}

/// Extends each request/response type pair with the side marker type and schema method name.
pub trait AcpMethod {
    fn method_name(&self) -> &'static str;
}

/// Connect together ACP request and response types for each rpc method.
pub trait AcpRequest: Clone + fmt::Debug + Serialize + AcpMethod {
    type Response: Clone + fmt::Debug + Serialize;
}

/// Contains an ACP request and a oneshot channel where the response of a matching type can be sent.
pub struct AcpArgsGeneric<T: AcpRequest, S: StorageMarker> {
    pub request: S::Type<T>,
    pub response_tx: oneshot::Sender<AcpResult<T::Response>>,
}

impl<T: AcpRequest, S: StorageMarker> Deref for AcpArgsGeneric<T, S> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.request.borrow()
    }
}

impl<T: AcpRequest, S: StorageMarker> fmt::Debug for AcpArgsGeneric<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.request.borrow())
    }
}

impl<T: AcpRequest, S: StorageMarker> AcpMethod for AcpArgsGeneric<T, S> {
    fn method_name(&self) -> &'static str {
        self.request.borrow().method_name()
    }
}

#[allow(type_alias_bounds)]
pub type AcpArgs<T: AcpRequest> = AcpArgsGeneric<T, Unboxed>;
#[allow(type_alias_bounds)]
pub type AcpArgsBox<T: AcpRequest> = AcpArgsGeneric<T, Boxed>;

impl<T: AcpRequest> AcpArgs<T> {
    pub fn boxed(self) -> AcpArgsBox<T> {
        AcpArgsBox {
            request: Box::new(self.request),
            response_tx: self.response_tx,
        }
    }
}

macro_rules! acp_define_request_response {
    ($request:ty, $response:ty, $method:expr $(,)?) => {
        impl AcpRequest for $request {
            type Response = $response;
        }

        impl AcpMethod for $request {
            fn method_name(&self) -> &'static str {
                $method
            }
        }
    };
}

acp_define_request_response!(acp::ExtRequest, acp::ExtResponse, "ext_method");
acp_define_request_response!(acp::ExtNotification, (), "ext_notification");

pub trait StorageMarker: fmt::Debug + Clone + Copy {
    type Type<T>: Borrow<T> + From<T>;
}

#[derive(Debug, Clone, Copy)]
pub struct Unboxed;
#[derive(Debug, Clone, Copy)]
pub struct Boxed;

impl StorageMarker for Unboxed {
    type Type<T> = T;
}

impl StorageMarker for Boxed {
    type Type<T> = Box<T>;
}

mod client {
    use futures::{FutureExt as _, future::LocalBoxFuture};

    use super::*;

    acp_define_request_response!(
        acp::RequestPermissionRequest,
        acp::RequestPermissionResponse,
        acp::CLIENT_METHOD_NAMES.session_request_permission,
    );
    acp_define_request_response!(
        acp::ReadTextFileRequest,
        acp::ReadTextFileResponse,
        acp::CLIENT_METHOD_NAMES.fs_read_text_file,
    );
    acp_define_request_response!(
        acp::WriteTextFileRequest,
        acp::WriteTextFileResponse,
        acp::CLIENT_METHOD_NAMES.fs_write_text_file,
    );
    acp_define_request_response!(
        acp::SessionNotification,
        (),
        acp::CLIENT_METHOD_NAMES.session_update,
    );
    acp_define_request_response!(
        acp::CreateTerminalRequest,
        acp::CreateTerminalResponse,
        acp::CLIENT_METHOD_NAMES.terminal_create,
    );
    acp_define_request_response!(
        acp::TerminalOutputRequest,
        acp::TerminalOutputResponse,
        acp::CLIENT_METHOD_NAMES.terminal_output,
    );
    acp_define_request_response!(
        acp::ReleaseTerminalRequest,
        acp::ReleaseTerminalResponse,
        acp::CLIENT_METHOD_NAMES.terminal_release,
    );
    acp_define_request_response!(
        acp::WaitForTerminalExitRequest,
        acp::WaitForTerminalExitResponse,
        acp::CLIENT_METHOD_NAMES.terminal_wait_for_exit,
    );
    acp_define_request_response!(
        acp::KillTerminalRequest,
        acp::KillTerminalResponse,
        acp::CLIENT_METHOD_NAMES.terminal_kill,
    );

    /// ACP messages meant *for* the client.
    #[derive(Debug, From)]
    pub enum AcpClientMessageGeneric<S: StorageMarker> {
        RequestPermission(AcpArgsGeneric<acp::RequestPermissionRequest, S>),
        ReadTextFile(AcpArgsGeneric<acp::ReadTextFileRequest, S>),
        WriteTextFile(AcpArgsGeneric<acp::WriteTextFileRequest, S>),
        SessionNotification(AcpArgsGeneric<acp::SessionNotification, S>),
        CreateTerminal(AcpArgsGeneric<acp::CreateTerminalRequest, S>),
        TerminalOutput(AcpArgsGeneric<acp::TerminalOutputRequest, S>),
        ReleaseTerminal(AcpArgsGeneric<acp::ReleaseTerminalRequest, S>),
        WaitForTerminalExit(AcpArgsGeneric<acp::WaitForTerminalExitRequest, S>),
        KillTerminalCommand(AcpArgsGeneric<acp::KillTerminalRequest, S>),
        ExtMethod(AcpArgsGeneric<acp::ExtRequest, S>),
        ExtNotification(AcpArgsGeneric<acp::ExtNotification, S>),
    }

    #[allow(type_alias_bounds)]
    pub type AcpClientMessage = AcpClientMessageGeneric<Unboxed>;
    #[allow(type_alias_bounds)]
    pub type AcpClientMessageBox = AcpClientMessageGeneric<Boxed>;

    impl<S: StorageMarker> AcpMethod for AcpClientMessageGeneric<S> {
        fn method_name(&self) -> &'static str {
            match self {
                Self::RequestPermission(a) => a.method_name(),
                Self::ReadTextFile(a) => a.method_name(),
                Self::WriteTextFile(a) => a.method_name(),
                Self::SessionNotification(a) => a.method_name(),
                Self::CreateTerminal(a) => a.method_name(),
                Self::TerminalOutput(a) => a.method_name(),
                Self::ReleaseTerminal(a) => a.method_name(),
                Self::WaitForTerminalExit(a) => a.method_name(),
                Self::KillTerminalCommand(a) => a.method_name(),
                Self::ExtMethod(a) => a.method_name(),
                Self::ExtNotification(a) => a.method_name(),
            }
        }
    }

    impl AcpClientMessage {
        pub fn boxed(self) -> AcpClientMessageBox {
            match self {
                Self::RequestPermission(args) => {
                    AcpClientMessageBox::RequestPermission(args.boxed())
                }
                Self::ReadTextFile(args) => AcpClientMessageBox::ReadTextFile(args.boxed()),
                Self::WriteTextFile(args) => AcpClientMessageBox::WriteTextFile(args.boxed()),
                Self::SessionNotification(args) => {
                    AcpClientMessageBox::SessionNotification(args.boxed())
                }
                Self::CreateTerminal(args) => AcpClientMessageBox::CreateTerminal(args.boxed()),
                Self::TerminalOutput(args) => AcpClientMessageBox::TerminalOutput(args.boxed()),
                Self::ReleaseTerminal(args) => AcpClientMessageBox::ReleaseTerminal(args.boxed()),
                Self::WaitForTerminalExit(args) => {
                    AcpClientMessageBox::WaitForTerminalExit(args.boxed())
                }
                Self::KillTerminalCommand(args) => {
                    AcpClientMessageBox::KillTerminalCommand(args.boxed())
                }
                Self::ExtMethod(args) => AcpClientMessageBox::ExtMethod(args.boxed()),
                Self::ExtNotification(args) => AcpClientMessageBox::ExtNotification(args.boxed()),
            }
        }

        pub fn route_to_client(
            self,
            client: impl acp::Client + 'static, // note: acp::Client is auto-implemented for Rc/Arc
            spawn: impl Fn(LocalBoxFuture<'static, ()>) + 'static,
        ) {
            match self {
                AcpClientMessage::RequestPermission(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.request_permission(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::ReadTextFile(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.read_text_file(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::WriteTextFile(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.write_text_file(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::SessionNotification(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.session_notification(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::CreateTerminal(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.create_terminal(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::TerminalOutput(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.terminal_output(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::ReleaseTerminal(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.release_terminal(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::WaitForTerminalExit(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.wait_for_terminal_exit(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::KillTerminalCommand(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.kill_terminal(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::ExtMethod(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.ext_method(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpClientMessage::ExtNotification(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(client.ext_notification(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
            }
        }
    }
}

mod agent {
    use futures::{FutureExt as _, future::LocalBoxFuture};

    use super::*;

    acp_define_request_response!(
        acp::InitializeRequest,
        acp::InitializeResponse,
        acp::AGENT_METHOD_NAMES.initialize,
    );
    acp_define_request_response!(
        acp::AuthenticateRequest,
        acp::AuthenticateResponse,
        acp::AGENT_METHOD_NAMES.authenticate,
    );
    acp_define_request_response!(
        acp::NewSessionRequest,
        acp::NewSessionResponse,
        acp::AGENT_METHOD_NAMES.session_new,
    );
    acp_define_request_response!(
        acp::LoadSessionRequest,
        acp::LoadSessionResponse,
        acp::AGENT_METHOD_NAMES.session_load,
    );
    acp_define_request_response!(
        acp::SetSessionModeRequest,
        acp::SetSessionModeResponse,
        acp::AGENT_METHOD_NAMES.session_set_mode,
    );
    acp_define_request_response!(
        acp::PromptRequest,
        acp::PromptResponse,
        acp::AGENT_METHOD_NAMES.session_prompt,
    );
    acp_define_request_response!(
        acp::CancelNotification,
        (),
        acp::AGENT_METHOD_NAMES.session_cancel,
    );
    acp_define_request_response!(
        acp::SetSessionModelRequest,
        acp::SetSessionModelResponse,
        acp::AGENT_METHOD_NAMES.session_set_model,
    );

    /// ACP messages meant *for* the agent.
    #[derive(Debug, From)]
    pub enum AcpAgentMessageGeneric<S: StorageMarker> {
        Initialize(AcpArgsGeneric<acp::InitializeRequest, S>),
        Authenticate(AcpArgsGeneric<acp::AuthenticateRequest, S>),
        NewSession(AcpArgsGeneric<acp::NewSessionRequest, S>),
        LoadSession(AcpArgsGeneric<acp::LoadSessionRequest, S>),
        SetSessionMode(AcpArgsGeneric<acp::SetSessionModeRequest, S>),
        Prompt(AcpArgsGeneric<acp::PromptRequest, S>),
        Cancel(AcpArgsGeneric<acp::CancelNotification, S>),
        ExtMethod(AcpArgsGeneric<acp::ExtRequest, S>),
        ExtNotification(AcpArgsGeneric<acp::ExtNotification, S>),
        SetSessionModel(AcpArgsGeneric<acp::SetSessionModelRequest, S>),
    }

    #[allow(type_alias_bounds)]
    pub type AcpAgentMessage = AcpAgentMessageGeneric<Unboxed>;
    #[allow(type_alias_bounds)]
    pub type AcpAgentMessageBox = AcpAgentMessageGeneric<Boxed>;

    impl<S: StorageMarker> AcpMethod for AcpAgentMessageGeneric<S> {
        fn method_name(&self) -> &'static str {
            match self {
                Self::Initialize(a) => a.method_name(),
                Self::Authenticate(a) => a.method_name(),
                Self::NewSession(a) => a.method_name(),
                Self::LoadSession(a) => a.method_name(),
                Self::SetSessionMode(a) => a.method_name(),
                Self::Prompt(a) => a.method_name(),
                Self::Cancel(a) => a.method_name(),
                Self::ExtMethod(a) => a.method_name(),
                Self::ExtNotification(a) => a.method_name(),
                Self::SetSessionModel(a) => a.method_name(),
            }
        }
    }

    impl<S: StorageMarker> Serialize for AcpAgentMessageGeneric<S> {
        fn serialize<Ser>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error>
        where
            Ser: serde::Serializer,
        {
            let mut state = serializer.serialize_struct("AcpAgentMessage", 2)?;
            state.serialize_field("method_name", self.method_name())?;
            match self {
                Self::Initialize(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
                Self::Authenticate(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
                Self::NewSession(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
                Self::LoadSession(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
                Self::SetSessionMode(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
                Self::Prompt(args) => state.serialize_field("request", args.request.borrow())?,
                Self::Cancel(args) => state.serialize_field("request", args.request.borrow())?,
                Self::ExtMethod(args) => state.serialize_field("request", args.request.borrow())?,
                Self::ExtNotification(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
                Self::SetSessionModel(args) => {
                    state.serialize_field("request", args.request.borrow())?
                }
            }
            state.end()
        }
    }

    impl<'de> Deserialize<'de> for AcpAgentMessage {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            #[derive(Deserialize)]
            struct RawMessage {
                method_name: String,
                request: serde_json::Value,
            }

            let raw = RawMessage::deserialize(deserializer)?;
            let method = raw.method_name.as_str();

            macro_rules! parse {
                ($variant:ident) => {{
                    let (response_tx, _) = oneshot::channel();
                    Ok(Self::$variant(AcpArgs {
                        request: serde_json::from_value(raw.request)
                            .map_err(serde::de::Error::custom)?,
                        response_tx,
                    }))
                }};
            }

            if method == acp::AGENT_METHOD_NAMES.initialize {
                parse!(Initialize)
            } else if method == acp::AGENT_METHOD_NAMES.authenticate {
                parse!(Authenticate)
            } else if method == acp::AGENT_METHOD_NAMES.session_new {
                parse!(NewSession)
            } else if method == acp::AGENT_METHOD_NAMES.session_load {
                parse!(LoadSession)
            } else if method == acp::AGENT_METHOD_NAMES.session_set_mode {
                parse!(SetSessionMode)
            } else if method == acp::AGENT_METHOD_NAMES.session_prompt {
                parse!(Prompt)
            } else if method == acp::AGENT_METHOD_NAMES.session_cancel {
                parse!(Cancel)
            } else if method == acp::AGENT_METHOD_NAMES.session_set_model {
                parse!(SetSessionModel)
            } else if method == "ext_method" {
                parse!(ExtMethod)
            } else if method == "ext_notification" {
                parse!(ExtNotification)
            } else {
                Err(serde::de::Error::custom(format!(
                    "Unknown method name: {method}"
                )))
            }
        }
    }

    impl AcpAgentMessage {
        pub fn boxed(self) -> AcpAgentMessageBox {
            match self {
                Self::Initialize(args) => AcpAgentMessageBox::Initialize(args.boxed()),
                Self::Authenticate(args) => AcpAgentMessageBox::Authenticate(args.boxed()),
                Self::NewSession(args) => AcpAgentMessageBox::NewSession(args.boxed()),
                Self::LoadSession(args) => AcpAgentMessageBox::LoadSession(args.boxed()),
                Self::SetSessionMode(args) => AcpAgentMessageBox::SetSessionMode(args.boxed()),
                Self::Prompt(args) => AcpAgentMessageBox::Prompt(args.boxed()),
                Self::Cancel(args) => AcpAgentMessageBox::Cancel(args.boxed()),
                Self::ExtMethod(args) => AcpAgentMessageBox::ExtMethod(args.boxed()),
                Self::ExtNotification(args) => AcpAgentMessageBox::ExtNotification(args.boxed()),
                Self::SetSessionModel(args) => AcpAgentMessageBox::SetSessionModel(args.boxed()),
            }
        }

        pub fn route_to_agent(
            self,
            agent: impl acp::Agent + 'static, // note: acp::Agent is auto-implemented for Rc/Arc
            spawn: impl Fn(LocalBoxFuture<'static, ()>) + 'static,
        ) {
            match self {
                AcpAgentMessage::Initialize(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.initialize(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::Authenticate(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.authenticate(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::NewSession(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.new_session(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::LoadSession(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.load_session(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::SetSessionMode(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.set_session_mode(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::Prompt(args) => spawn(
                    async move {
                        _ = args.response_tx.send(agent.prompt(args.request).await).ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::Cancel(args) => spawn(
                    async move {
                        _ = args.response_tx.send(agent.cancel(args.request).await).ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::ExtMethod(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.ext_method(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::ExtNotification(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.ext_notification(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
                AcpAgentMessage::SetSessionModel(args) => spawn(
                    async move {
                        _ = args
                            .response_tx
                            .send(agent.set_session_model(args.request).await)
                            .ok();
                    }
                    .boxed_local(),
                ),
            }
        }
    }
}
