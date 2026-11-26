//! Shared upload utilities for session persistence and agent telemetry.
//!
//! This module provides a unified interface for uploading bytes to cloud storage,
//! supporting direct upload (via service account), proxy upload (via cli-chat-proxy),
//! and S3-compatible backends.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

use crate::UploadMethod;
use echo_agent_auth::{AuthCredentialProvider, StaticAuthCredentialProvider};

use crate::storage_client::{Auth401AttributionCallback, StaticEchoAgentAuth, StorageClient};

/// Threshold for switching to multipart upload (50 MB).
///
/// Files larger than this use `StorageClient::upload_multipart()` (signed URLs,
/// parts uploaded directly to cloud storage) instead of streaming through the proxy.
pub const MULTIPART_UPLOAD_THRESHOLD: u64 = 50 * 1024 * 1024;

/// Construct a `StorageClient` for proxy-mode uploads. Uses the caller-provided
/// refresh-aware credentials when present, otherwise falls back to a
/// `StaticEchoAgentAuth` carrying the inline user / deployment keys from
/// `UploadMethod::Proxy`. The optional `http_client` lets the caller pass a
/// shell-tuned client (HTTP/2 keep-alive, conn pool tuning); when `None` we
/// fall back to `reqwest::Client::new()`.
fn build_proxy_client_with_fallback(
    proxy_base_url: &str,
    user_token: &str,
    deployment_key: Option<String>,
    credentials: Option<Arc<dyn AuthCredentialProvider>>,
    attribution: Option<Arc<dyn Auth401AttributionCallback>>,
    http_client: Option<reqwest::Client>,
) -> StorageClient {
    let provider = credentials.unwrap_or_else(|| {
        let mut creds = StaticEchoAgentAuth::new(Some(user_token.to_owned()));
        creds.deployment_key = deployment_key;
        let bearer = creds.wire_bearer();
        Arc::new(StaticAuthCredentialProvider::new(Box::new(creds), bearer))
    });
    let http_client = http_client.unwrap_or_default();
    let mut client = StorageClient::with_provider(proxy_base_url, http_client, provider);
    if let Some(cb) = attribution {
        client = client.with_attribution(cb);
    }
    client
}

/// Implement `StorageConfig` for `TraceExportConfig`. Lives here (alongside the
/// trait + upload helpers) so callers can use the shared upload helpers without
/// a foreign-trait impl. Refresh-aware callers still get credential /
/// attribution wiring via `TraceExportConfigWithAuth` (in shell).
impl StorageConfig for crate::TraceExportConfig {
    fn bucket_url(&self) -> &str {
        // For proxy mode, bucket_url may be None (proxy determines it from ACLs).
        // Return a placeholder that won't be used.
        self.bucket_url.as_deref().unwrap_or("gs://placeholder")
    }

    fn upload_method(&self) -> &UploadMethod {
        &self.upload_method
    }
}

/// A trait for storage configuration that provides bucket URL and upload method.
/// This allows different config types (TraceExportConfig, etc.) to share upload logic.
pub trait StorageConfig {
    fn bucket_url(&self) -> &str;
    fn upload_method(&self) -> &UploadMethod;
    /// Optional refresh-aware credentials for proxy-mode uploads. When
    /// `Some(_)`, `upload_*_via_proxy` helpers construct a `StorageClient`
    /// via `StorageClient::with_provider(...)` so 401 retries can request
    /// a token refresh. Default `None` for configs that ship a static
    /// user-token only.
    fn proxy_credentials(&self) -> Option<Arc<dyn AuthCredentialProvider>> {
        None
    }
    /// Optional 401-attribution callback. When `Some(_)`, the constructed
    /// `StorageClient` also calls `with_attribution(...)` so the embedding
    /// application records auth-attribution telemetry for proxy 401s.
    fn proxy_attribution(&self) -> Option<Arc<dyn Auth401AttributionCallback>> {
        None
    }
    /// Optional HTTP client for proxy-mode uploads. `None` falls back to
    /// `reqwest::Client::new()` (used by bins/tests). Production callers
    /// should return shell's tuned `shared_upload_client()` -- HTTP/2
    /// keep-alive + aggressive connection pool eviction. The trace upload
    /// queue, feedback uploads, share uploads, and subagent metadata
    /// uploads all rely on this tuning to avoid stale-connection retries
    /// during backoff loops.
    fn proxy_http_client(&self) -> Option<reqwest::Client> {
        None
    }
}

/// Uploads bytes to cloud storage at the specified path.
/// Returns the full storage URL on success.
/// Dispatches to direct, proxy, or S3 backend based on config.
pub async fn upload_bytes<C: StorageConfig>(
    config: &C,
    object_path: &str,
    content: &[u8],
    content_type: &str,
) -> anyhow::Result<String> {
    match config.upload_method() {
        UploadMethod::Direct {
            service_account_key,
        } => {
            // Parse the bucket URL to extract bucket name (required for direct mode)
            let url = url::Url::parse(config.bucket_url())
                .with_context(|| format!("Invalid GCS URL: {}", config.bucket_url()))?;

            if url.scheme() != "gs" {
                anyhow::bail!(
                    "Invalid GCS URL scheme: expected 'gs', got '{}'",
                    url.scheme()
                );
            }

            let bucket = url
                .host_str()
                .context("GCS URL must have a bucket name")?
                .to_string();

            upload_bytes_direct(
                &bucket,
                object_path,
                content,
                content_type,
                service_account_key.as_deref(),
            )
            .await
        }
        UploadMethod::Proxy {
            proxy_base_url,
            user_token,
            deployment_key,
            alpha_test_key: _,
        } => {
            // For proxy mode, bucket is determined by proxy from user ACLs
            tracing::debug!(
                proxy_base_url = %proxy_base_url,
                object_path = %object_path,
                "Uploading bytes to GCS via proxy (bucket determined by proxy from ACLs)"
            );
            upload_bytes_via_proxy(
                proxy_base_url,
                user_token,
                deployment_key.as_deref(),
                object_path,
                content,
                content_type,
                config.proxy_credentials(),
                config.proxy_attribution(),
                config.proxy_http_client(),
            )
            .await
        }
        UploadMethod::S3 {
            bucket,
            region,
            credentials_file,
            credentials_content,
            endpoint_url,
        } => {
            crate::s3::upload_bytes(
                bucket,
                object_path,
                content,
                content_type,
                region,
                credentials_content.as_deref(),
                credentials_file.as_deref(),
                endpoint_url.as_deref(),
            )
            .await
        }
    }
}

/// Like [`upload_bytes`], but in proxy mode uses a pre-signed PUT URL
/// so the data goes directly to storage instead of through the proxy.
///
/// This avoids the nginx `proxy-body-size: 4m` limit on the HTTP ingress and
/// the Cloudflare 100 MB limit, making it safe for arbitrarily large payloads
/// (e.g. session share data).
///
/// In direct mode this is identical to `upload_bytes` (the service
/// account already talks to storage directly).
pub async fn upload_bytes_signed<C: StorageConfig>(
    config: &C,
    object_path: &str,
    content: &[u8],
    content_type: &str,
) -> anyhow::Result<String> {
    match config.upload_method() {
        UploadMethod::Direct { .. } => {
            // Direct mode already bypasses the proxy — reuse the existing path.
            upload_bytes(config, object_path, content, content_type).await
        }
        UploadMethod::Proxy {
            proxy_base_url,
            user_token,
            deployment_key,
            alpha_test_key: _,
        } => {
            tracing::debug!(
                proxy_base_url = %proxy_base_url,
                object_path = %object_path,
                bytes = content.len(),
                "Uploading bytes to GCS via signed URL (bypasses proxy body limits)"
            );
            upload_bytes_via_signed_url(
                proxy_base_url,
                user_token,
                deployment_key.as_deref(),
                object_path,
                content,
                content_type,
                config.proxy_credentials(),
                config.proxy_attribution(),
                config.proxy_http_client(),
            )
            .await
        }
        UploadMethod::S3 { .. } => upload_bytes(config, object_path, content, content_type).await,
    }
}

/// Uploads a file to cloud storage by streaming from disk.
///
/// Preferred over `upload_bytes` for the background upload queue because:
/// - Never loads the full file into memory (critical for multi-GB dedup blobs)
/// - For Proxy mode with large files (>50 MB), uses signed-URL multipart upload
///   so data travels directly to storage, bypassing the proxy's body size limits
/// - For Proxy mode with small files, uses `StorageClient::upload_file()` (streaming)
/// - For Direct mode, streams via the gcloud-storage crate
pub async fn upload_file<C: StorageConfig>(
    config: &C,
    object_path: &str,
    file_path: &Path,
    content_type: &str,
) -> anyhow::Result<String> {
    match config.upload_method() {
        UploadMethod::Direct {
            service_account_key,
        } => {
            let bucket_url = config.bucket_url();
            let url = url::Url::parse(bucket_url)
                .with_context(|| format!("Invalid GCS URL: {}", bucket_url))?;
            if url.scheme() != "gs" {
                anyhow::bail!(
                    "Invalid GCS URL scheme: expected 'gs', got '{}'",
                    url.scheme()
                );
            }
            let bucket = url
                .host_str()
                .context("GCS URL must have a bucket name")?
                .to_string();
            upload_file_direct(
                &bucket,
                object_path,
                file_path,
                content_type,
                service_account_key.as_deref(),
            )
            .await
        }
        UploadMethod::Proxy {
            proxy_base_url,
            user_token,
            deployment_key,
            alpha_test_key: _,
        } => {
            upload_file_via_proxy(
                proxy_base_url,
                user_token,
                deployment_key.as_deref(),
                object_path,
                file_path,
                content_type,
                config.proxy_credentials(),
                config.proxy_attribution(),
                config.proxy_http_client(),
            )
            .await
        }
        UploadMethod::S3 {
            bucket,
            region,
            credentials_file,
            credentials_content,
            endpoint_url,
        } => {
            crate::s3::upload_file(
                bucket,
                object_path,
                file_path,
                content_type,
                region,
                credentials_content.as_deref(),
                credentials_file.as_deref(),
                endpoint_url.as_deref(),
            )
            .await
        }
    }
}

/// Uploads an async reader to cloud storage, dispatching to the appropriate backend.
///
/// Used for streaming compressed uploads where the reader is consumed once per attempt.
/// Callers handle retries by recreating the reader.
pub async fn upload_stream<C: StorageConfig, R>(
    config: &C,
    object_path: &str,
    reader: R,
    content_type: &str,
) -> anyhow::Result<String>
where
    R: tokio::io::AsyncRead + Send + Sync + 'static,
{
    match config.upload_method() {
        UploadMethod::Direct {
            service_account_key,
        } => {
            let bucket_url = config.bucket_url();
            let url = url::Url::parse(bucket_url)
                .with_context(|| format!("Invalid GCS URL: {}", bucket_url))?;
            if url.scheme() != "gs" {
                anyhow::bail!(
                    "Invalid GCS URL scheme: expected 'gs', got '{}'",
                    url.scheme()
                );
            }
            let bucket = url
                .host_str()
                .context("GCS URL must have a bucket name")?
                .to_string();
            upload_stream_direct(
                &bucket,
                object_path,
                reader,
                content_type,
                service_account_key.as_deref(),
            )
            .await
        }
        UploadMethod::Proxy {
            proxy_base_url,
            user_token,
            deployment_key,
            alpha_test_key: _,
        } => {
            let storage_client = build_proxy_client_with_fallback(
                proxy_base_url,
                user_token,
                deployment_key.as_deref().map(|s| s.to_owned()),
                config.proxy_credentials(),
                config.proxy_attribution(),
                config.proxy_http_client(),
            );
            let response = storage_client
                .upload_stream(object_path, reader, content_type)
                .await
                .with_context(|| format!("Streaming upload failed for {}", object_path))?;
            Ok(format!("gs://{}/{}", response.bucket, response.path))
        }
        UploadMethod::S3 {
            bucket,
            region,
            credentials_file,
            credentials_content,
            endpoint_url,
        } => {
            crate::s3::upload_stream(
                bucket,
                object_path,
                reader,
                content_type,
                region,
                credentials_content.as_deref(),
                credentials_file.as_deref(),
                endpoint_url.as_deref(),
            )
            .await
        }
    }
}

/// Stream an async reader directly to GCS via the gcloud-storage client.
async fn upload_stream_direct<R: tokio::io::AsyncRead + Send + Sync + 'static>(
    bucket: &str,
    object_path: &str,
    reader: R,
    content_type: &str,
    service_account_key: Option<&str>,
) -> anyhow::Result<String> {
    use gcloud_storage::http::objects::upload::{Media, UploadObjectRequest, UploadType};
    use tokio_util::io::ReaderStream;

    let client = build_gcs_client(service_account_key).await?;
    let stream = ReaderStream::new(reader);

    let mut media = Media::new(object_path.to_string());
    media.content_type = content_type.to_owned().into();
    let upload_type = UploadType::Simple(media);
    let request = UploadObjectRequest {
        bucket: bucket.to_string(),
        ..Default::default()
    };
    client
        .upload_streamed_object(&request, stream, &upload_type)
        .await
        .with_context(|| format!("Failed to upload to gs://{}/{}", bucket, object_path))?;

    Ok(format!("gs://{}/{}", bucket, object_path))
}

/// Upload a file through the cli-chat-proxy, choosing multipart vs streaming based on size.
///
/// Files > `MULTIPART_UPLOAD_THRESHOLD` use signed-URL multipart upload (parts go
/// directly to cloud storage, not through the proxy HTTP body). This avoids the proxy's request
/// body size limit and the timeout issues that cause 55% of upload failures for large
/// dedup blobs.
async fn upload_file_via_proxy(
    proxy_base_url: &str,
    user_token: &str,
    deployment_key: Option<&str>,
    object_path: &str,
    file_path: &Path,
    content_type: &str,
    credentials: Option<Arc<dyn AuthCredentialProvider>>,
    attribution: Option<Arc<dyn Auth401AttributionCallback>>,
    http_client: Option<reqwest::Client>,
) -> anyhow::Result<String> {
    use crate::storage_client::{MultipartUploadOptions, RetryConfig};

    let storage_client = build_proxy_client_with_fallback(
        proxy_base_url,
        user_token,
        deployment_key.map(|s| s.to_owned()),
        credentials,
        attribution,
        http_client,
    )
    .with_retry_config(RetryConfig::conservative());

    let file_size = tokio::fs::metadata(file_path)
        .await
        .with_context(|| format!("Failed to get file metadata: {}", file_path.display()))?
        .len();

    if file_size > MULTIPART_UPLOAD_THRESHOLD {
        // Large file: upload directly to cloud storage via signed URLs (bypasses proxy body)
        tracing::info!(
            file_size,
            threshold = MULTIPART_UPLOAD_THRESHOLD,
            upload_method = "multipart",
            path = %file_path.display(),
            "Upload queue: using multipart for large file"
        );
        let options = MultipartUploadOptions::new().with_max_concurrent(4);
        let response = storage_client
            .upload_multipart(object_path, file_path, content_type, Some(options))
            .await
            .with_context(|| format!("Multipart upload failed for {}", object_path))?;
        Ok(response.gcs_url)
    } else {
        // Small file: stream through proxy (no memory copy)
        tracing::debug!(
            file_size,
            upload_method = "streaming",
            path = %file_path.display(),
            "Upload queue: using streaming for small file"
        );
        let response = storage_client
            .upload_file(object_path, file_path, content_type)
            .await
            .with_context(|| format!("Streaming upload failed for {}", object_path))?;
        Ok(format!("gs://{}/{}", response.bucket, response.path))
    }
}

/// Build a GCS client with optional service account key, or default ADC.
async fn build_gcs_client(
    service_account_key: Option<&str>,
) -> anyhow::Result<gcloud_storage::client::Client> {
    use gcloud_storage::client::{Client as GcsClient, ClientConfig as GcsClientConfig};

    let gcs_config = if let Some(key_json) = service_account_key {
        GcsClientConfig::default()
            .with_credentials(
                gcloud_storage::client::google_cloud_auth::credentials::CredentialsFile::new_from_str(key_json)
                    .await
                    .context("Failed to parse service account key")?,
            )
            .await
            .context("Failed to configure GCS client with service account")?
    } else {
        GcsClientConfig::default()
            .with_auth()
            .await
            .context("Failed to authenticate GCS client")?
    };

    Ok(GcsClient::new(gcs_config))
}

/// Upload a file directly to GCS by streaming from disk.
async fn upload_file_direct(
    bucket: &str,
    object_path: &str,
    file_path: &Path,
    content_type: &str,
    service_account_key: Option<&str>,
) -> anyhow::Result<String> {
    use gcloud_storage::http::objects::upload::{Media, UploadObjectRequest, UploadType};
    use tokio::fs::File as TokioFile;
    use tokio_util::io::ReaderStream;

    let client = build_gcs_client(service_account_key).await?;

    let file = TokioFile::open(file_path)
        .await
        .with_context(|| format!("Failed to open file: {}", file_path.display()))?;
    // ReaderStream<TokioFile> yields io::Result<Bytes>; io::Error satisfies
    // upload_streamed_object's S::Error: Into<Box<dyn Error + Send + Sync>> bound directly.
    let stream = ReaderStream::new(file);

    let mut media = Media::new(object_path.to_string());
    media.content_type = content_type.to_owned().into();
    let upload_type = UploadType::Simple(media);
    let request = UploadObjectRequest {
        bucket: bucket.to_string(),
        ..Default::default()
    };
    client
        .upload_streamed_object(&request, stream, &upload_type)
        .await
        .with_context(|| format!("Failed to upload to gs://{}/{}", bucket, object_path))?;

    Ok(format!("gs://{}/{}", bucket, object_path))
}

/// Uploads bytes directly to GCS using the gcloud-storage client.
async fn upload_bytes_direct(
    bucket: &str,
    object_path: &str,
    content: &[u8],
    content_type: &str,
    service_account_key: Option<&str>,
) -> anyhow::Result<String> {
    use gcloud_storage::http::objects::upload::{Media, UploadObjectRequest, UploadType};

    let client = build_gcs_client(service_account_key).await?;

    let mut media = Media::new(object_path.to_string());
    media.content_type = content_type.to_owned().into();
    let upload_type = UploadType::Simple(media);
    let request = UploadObjectRequest {
        bucket: bucket.to_string(),
        ..Default::default()
    };

    client
        .upload_object(&request, content.to_vec(), &upload_type)
        .await
        .with_context(|| format!("Failed to upload to gs://{}/{}", bucket, object_path))?;

    // Return the full GCS URL
    Ok(format!("gs://{}/{}", bucket, object_path))
}

/// Uploads bytes via the cli-chat-proxy storage proxy API.
/// The bucket is determined by the proxy based on the user's ACLs.
async fn upload_bytes_via_proxy(
    proxy_base_url: &str,
    user_token: &str,
    deployment_key: Option<&str>,
    object_path: &str,
    content: &[u8],
    content_type: &str,
    credentials: Option<Arc<dyn AuthCredentialProvider>>,
    attribution: Option<Arc<dyn Auth401AttributionCallback>>,
    http_client: Option<reqwest::Client>,
) -> anyhow::Result<String> {
    use crate::storage_client::RetryConfig;

    // Conservative retry config handles storage-backend 429 errors during autoscaling.
    let storage_client = build_proxy_client_with_fallback(
        proxy_base_url,
        user_token,
        deployment_key.map(|s| s.to_owned()),
        credentials,
        attribution,
        http_client,
    )
    .with_retry_config(RetryConfig::conservative());

    let response = storage_client
        .upload(object_path, content, content_type)
        .await
        .with_context(|| {
            format!(
                "Failed to upload to storage proxy: {} (path: {})",
                proxy_base_url, object_path
            )
        })?;

    // Return the full GCS URL
    Ok(format!("gs://{}/{}", response.bucket, response.path))
}

/// Uploads bytes to cloud storage via a pre-signed PUT URL obtained from the proxy.
///
/// This completely bypasses the proxy for the data transfer, avoiding
/// nginx / Cloudflare body-size limits.  The proxy is only contacted
/// once (to generate the signed URL), after which the bytes go straight
/// to cloud storage.
///
/// Use this when the payload may exceed 4 MB (the nginx `proxy-body-size`
/// on the HTTP ingress) — e.g. session share data.
pub async fn upload_bytes_via_signed_url(
    proxy_base_url: &str,
    user_token: &str,
    deployment_key: Option<&str>,
    object_path: &str,
    content: &[u8],
    content_type: &str,
    credentials: Option<Arc<dyn AuthCredentialProvider>>,
    attribution: Option<Arc<dyn Auth401AttributionCallback>>,
    http_client: Option<reqwest::Client>,
) -> anyhow::Result<String> {
    let storage_client = build_proxy_client_with_fallback(
        proxy_base_url,
        user_token,
        deployment_key.map(|s| s.to_owned()),
        credentials,
        attribution,
        http_client,
    );

    let signed = storage_client
        .upload_bytes_signed(object_path, content, content_type)
        .await
        .with_context(|| {
            format!(
                "Failed to upload via signed URL: {} (path: {})",
                proxy_base_url, object_path
            )
        })?;

    Ok(format!("gs://{}/{}", signed.bucket, signed.path))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TraceExportConfig, UploadMethod};

    fn proxy_config() -> TraceExportConfig {
        proxy_config_with_url("https://proxy.example.com/v1".to_string())
    }

    fn proxy_config_with_url(base_url: String) -> TraceExportConfig {
        TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: base_url,
                user_token: "tok".to_string(),
                deployment_key: None,
                alpha_test_key: None,
            },
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
        }
    }

    fn direct_config() -> TraceExportConfig {
        TraceExportConfig {
            bucket_url: Some("gs://test-bucket".to_string()),
            service_account_key: None,
            upload_method: UploadMethod::Direct {
                service_account_key: None,
            },
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
        }
    }

    #[test]
    fn multipart_threshold_is_50mb() {
        assert_eq!(
            MULTIPART_UPLOAD_THRESHOLD,
            50 * 1024 * 1024,
            "Multipart threshold must be 50 MB to match the plan and repo_changes.rs"
        );
    }

    #[tokio::test]
    async fn upload_file_proxy_missing_file_returns_error() {
        // upload_file_via_proxy checks metadata before connecting — should fail
        // fast with a descriptive error if the temp file was deleted mid-flight.
        let config = proxy_config();
        let result = upload_file(
            &config,
            "session/turn_0/test.bin",
            std::path::Path::new("/tmp/nonexistent_upload_queue_test_file"),
            "application/octet-stream",
        )
        .await;
        assert!(result.is_err(), "Should error for missing file");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("metadata") || err.contains("No such file"),
            "Error should mention file metadata: {}",
            err
        );
    }

    #[tokio::test]
    async fn upload_file_direct_missing_file_returns_error() {
        // Direct mode tries to authenticate first — bucket URL parse should succeed,
        // but the file open will fail later. We only care it returns an error, not panics.
        let config = direct_config();
        let result = upload_file(
            &config,
            "session/turn_0/test.bin",
            std::path::Path::new("/tmp/nonexistent_upload_queue_test_file"),
            "application/octet-stream",
        )
        .await;
        assert!(result.is_err(), "Should error for missing file");
    }

    #[tokio::test]
    async fn upload_file_direct_invalid_scheme_returns_error() {
        // Verify that a non-gs:// URL is rejected before any I/O.
        let config = TraceExportConfig {
            bucket_url: Some("https://not-a-gcs-url.example.com".to_string()),
            service_account_key: None,
            upload_method: UploadMethod::Direct {
                service_account_key: None,
            },
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
        };
        let result = upload_file(
            &config,
            "path/test.bin",
            std::path::Path::new("/tmp/file"),
            "application/octet-stream",
        )
        .await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("gs"),
            "Error should mention expected scheme"
        );
    }

    /// Shared state for the dispatch test server, tracking which endpoints were hit.
    #[derive(Clone, Default)]
    struct DispatchState {
        multipart_called: std::sync::Arc<std::sync::atomic::AtomicBool>,
        storage_called: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    /// Start a minimal axum server (with proper State extractors) that records
    /// which upload routes were hit. Uses the same State extractor pattern as
    /// storage_client_tests.rs to ensure reliable flag updates in Bazel CI.
    ///
    /// Returns (addr, state) where state.multipart_called / state.storage_called
    /// are set to true when the respective route is hit.
    async fn start_dispatch_test_server() -> (std::net::SocketAddr, DispatchState) {
        use axum::{
            Router, body::Body, extract::State, http::StatusCode, response::IntoResponse,
            routing::post,
        };
        use std::sync::atomic::Ordering;
        use tokio::net::TcpListener;

        let state = DispatchState::default();

        async fn multipart_handler(
            State(s): State<DispatchState>,
            _body: Body,
        ) -> impl IntoResponse {
            s.multipart_called.store(true, Ordering::SeqCst);
            // 400 = non-retryable: client fails fast without backoff delays
            (StatusCode::BAD_REQUEST, r#"{"error":"test"}"#)
        }

        async fn storage_handler(State(s): State<DispatchState>, _body: Body) -> impl IntoResponse {
            s.storage_called.store(true, Ordering::SeqCst);
            (StatusCode::BAD_REQUEST, r#"{"error":"test"}"#)
        }

        let app = Router::new()
            .route("/v1/storage/multipart/init", post(multipart_handler))
            .route("/v1/storage", post(storage_handler))
            .with_state(state.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        // Give the server 50ms to bind and accept — more headroom for Bazel CI sandboxing.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        (addr, state)
    }

    #[tokio::test]
    async fn upload_file_via_proxy_uses_multipart_for_large_files() {
        // Large file (just over 50 MB threshold) should hit the multipart init endpoint.
        // Uses set_len() to create a sparse file — no actual disk write.
        let (addr, state) = start_dispatch_test_server().await;
        let config = proxy_config_with_url(format!("http://{}/v1", addr));

        let temp = tempfile::TempDir::new().unwrap();
        let large_file = temp.path().join("large.bin");
        let f = std::fs::File::create(&large_file).unwrap();
        f.set_len(MULTIPART_UPLOAD_THRESHOLD + 1).unwrap(); // sparse file, no actual disk write

        let _ = upload_file(
            &config,
            "session/turn_0/large.bin",
            &large_file,
            "application/octet-stream",
        )
        .await;

        assert!(
            state
                .multipart_called
                .load(std::sync::atomic::Ordering::SeqCst),
            "File > 50MB should use multipart upload"
        );
        assert!(
            !state
                .storage_called
                .load(std::sync::atomic::Ordering::SeqCst),
            "File > 50MB should NOT use the simple storage endpoint"
        );
    }

    #[tokio::test]
    async fn upload_file_via_proxy_uses_streaming_for_small_files() {
        // Small file (1 KB) should hit the simple storage endpoint, not multipart.
        let (addr, state) = start_dispatch_test_server().await;
        let config = proxy_config_with_url(format!("http://{}/v1", addr));

        let temp = tempfile::TempDir::new().unwrap();
        let small_file = temp.path().join("small.bin");
        std::fs::write(&small_file, vec![0u8; 1024]).unwrap();

        let _ = upload_file(
            &config,
            "session/turn_0/small.bin",
            &small_file,
            "application/octet-stream",
        )
        .await;

        assert!(
            !state
                .multipart_called
                .load(std::sync::atomic::Ordering::SeqCst),
            "File < 50MB should NOT use multipart upload"
        );
        assert!(
            state
                .storage_called
                .load(std::sync::atomic::Ordering::SeqCst),
            "File < 50MB should use the simple storage endpoint"
        );
    }
}
