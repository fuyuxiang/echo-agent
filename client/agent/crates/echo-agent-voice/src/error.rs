use thiserror::Error;

#[derive(Debug, Error)]
pub enum VoiceError {
    #[error("configuration: {0}")]
    Config(String),

    #[error("STT: {0}")]
    Stt(String),

    #[error("auth: {0}")]
    Auth(String),

    #[error("WebSocket: {0}")]
    WebSocket(String),
}
