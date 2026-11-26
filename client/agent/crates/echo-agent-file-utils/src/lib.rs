#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Local data collection: upload queueing and S3-compatible blob storage.
pub(crate) mod circuit_breaker_observer;
/// Wrap a raw client with [`echo_agent_auth::AuthRetryMiddleware`] for automatic 401 retry.
pub fn with_auth_retry(
    client: reqwest::Client,
    credentials: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider>,
) -> reqwest_middleware::ClientWithMiddleware {
    reqwest_middleware::ClientBuilder::new(client)
        .with(echo_agent_auth::AuthRetryMiddleware::new(credentials, 1))
        .build()
}
pub mod gcs;
pub mod queue;
pub mod s3;
pub mod storage_client;
pub mod trace_context;
pub mod upload_config;
pub mod workspace_classifier;
pub use upload_config::*;
/// Compute SHA256 hash of content as a hex string.
pub fn sha256_hex(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    format!("{:x}", hasher.finalize())
}
/// Compute SHA256 hash of a file by streaming, without loading entire file into memory.
/// If `max_bytes` is set (> 0), only hash up to that many bytes.
pub fn sha256_hex_from_file(
    path: &std::path::Path,
    max_bytes: Option<u64>,
) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut reader: Box<dyn Read> = if let Some(limit) = max_bytes {
        Box::new(file.take(limit))
    } else {
        Box::new(file)
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
