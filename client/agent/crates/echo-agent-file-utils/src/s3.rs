use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use futures::StreamExt;

/// Map an S3 HeadObject error to NotFound / Unauthorized / ProbeFailed.
/// `ServiceError` and `ResponseError` both carry a status; everything else
/// (construction, dispatch, timeout) is transient.
fn classify_head_error<E>(err: &aws_sdk_s3::error::SdkError<E>) -> HeadOutcome
where
    E: std::fmt::Debug,
{
    use aws_sdk_s3::error::SdkError;
    let status = match err {
        SdkError::ServiceError(ctx) => Some(ctx.raw().status().as_u16()),
        SdkError::ResponseError(ctx) => Some(ctx.raw().status().as_u16()),
        _ => None,
    };
    match status {
        Some(404) => HeadOutcome::NotFound,
        Some(401) | Some(403) => HeadOutcome::Unauthorized,
        _ => HeadOutcome::ProbeFailed,
    }
}

#[derive(Debug)]
enum HeadOutcome {
    NotFound,
    Unauthorized,
    ProbeFailed,
}

/// Some S3-compatible endpoints reject single PutObject chunks above 16 MiB.
/// Use multipart upload with 8 MiB parts to stay within that limit.
const MULTIPART_THRESHOLD: usize = 8 * 1024 * 1024;
const MULTIPART_PART_SIZE: usize = 8 * 1024 * 1024;

/// Parse credential content (JSON or INI format) into AWS SDK credentials.
fn parse_aws_credentials(content: &str) -> anyhow::Result<aws_sdk_s3::config::Credentials> {
    #[derive(serde::Deserialize)]
    struct JsonCreds {
        aws_access_key_id: String,
        aws_secret_access_key: String,
        #[serde(default)]
        aws_session_token: Option<String>,
    }

    if let Ok(parsed) = serde_json::from_str::<JsonCreds>(content) {
        return Ok(aws_sdk_s3::config::Credentials::new(
            &parsed.aws_access_key_id,
            &parsed.aws_secret_access_key,
            parsed.aws_session_token,
            None,
            "echo-agent-shell-trace-upload",
        ));
    }

    let strip_comment = |v: &str| {
        v.split_once('#')
            .map_or(v, |(before, _)| before)
            .trim()
            .to_owned()
    };
    let mut key_id = None;
    let mut secret = None;
    let mut token = None;
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "aws_access_key_id" => key_id = Some(strip_comment(v)),
                "aws_secret_access_key" => secret = Some(strip_comment(v)),
                "aws_session_token" => token = Some(strip_comment(v)),
                _ => {}
            }
        }
    }

    match (key_id, secret) {
        (Some(k), Some(s)) => Ok(aws_sdk_s3::config::Credentials::new(
            &k,
            &s,
            token,
            None,
            "echo-agent-shell-trace-upload",
        )),
        _ => anyhow::bail!(
            "AWS credentials are neither valid JSON \
             nor contain aws_access_key_id and aws_secret_access_key"
        ),
    }
}

/// Build an S3 client. Uses path-style addressing when `endpoint_url` is set.
///
/// Reads `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` environment
/// variables so that S3 traffic can route through a corporate HTTP proxy when
/// the S3-compatible endpoint is not directly reachable.
pub(crate) async fn build_s3_client(
    region: &str,
    credentials_content: Option<&str>,
    credentials_file: Option<&str>,
    endpoint_url: Option<&str>,
) -> anyhow::Result<aws_sdk_s3::Client> {
    let proxy_config = aws_smithy_http_client::proxy::ProxyConfig::from_env();
    let http_client = aws_smithy_http_client::Builder::new().build_with_connector_fn(
        move |settings, _runtime_components| {
            let mut builder =
                aws_smithy_http_client::Connector::builder().proxy_config(proxy_config.clone());
            if let Some(s) = settings {
                builder.set_connector_settings(Some(s.clone()));
            }
            builder
                .tls_provider(aws_smithy_http_client::tls::Provider::Rustls(
                    aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
                ))
                .build()
        },
    );

    let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .http_client(http_client)
        .region(aws_config::Region::new(region.to_owned()));

    let resolved_content = match (credentials_content, credentials_file) {
        (Some(inline), _) => Some(inline.to_owned()),
        (None, Some(path)) => Some(
            tokio::fs::read_to_string(path)
                .await
                .with_context(|| format!("Failed to read AWS credentials file: {path}"))?,
        ),
        (None, None) => None,
    };

    if let Some(ref content) = resolved_content {
        config_loader = config_loader.credentials_provider(parse_aws_credentials(content)?);
    } else if endpoint_url.is_some() {
        config_loader = config_loader.credentials_provider(aws_sdk_s3::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "echo-agent-shell-test",
        ));
    }

    let sdk_config = config_loader.load().await;
    let mut builder =
        aws_sdk_s3::config::Builder::from(&sdk_config).force_path_style(endpoint_url.is_some());
    if let Some(url) = endpoint_url {
        builder = builder.endpoint_url(url);
    }
    Ok(aws_sdk_s3::Client::from_conf(builder.build()))
}

/// Static access-key credentials for presigning S3 URLs.
///
/// `Debug` is intentionally redacted — the struct holds plaintext secrets.
#[derive(Clone)]
pub struct S3StaticCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for S3StaticCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3StaticCredentials")
            .field("access_key_id", &"[redacted]")
            .field("secret_access_key", &"[redacted]")
            .finish()
    }
}

impl S3StaticCredentials {
    fn to_credentials_content(&self) -> String {
        serde_json::json!({
            "aws_access_key_id": self.access_key_id,
            "aws_secret_access_key": self.secret_access_key,
        })
        .to_string()
    }
}

pub async fn presign_put_url(
    region: &str,
    endpoint_url: Option<&str>,
    creds: &S3StaticCredentials,
    bucket: &str,
    key: &str,
    content_type: &str,
    expires_in: std::time::Duration,
) -> anyhow::Result<String> {
    let content = creds.to_credentials_content();
    let client = build_s3_client(region, Some(&content), None, endpoint_url).await?;
    let presigning_config = aws_sdk_s3::presigning::PresigningConfig::expires_in(expires_in)?;
    let presigned = client
        .put_object()
        .bucket(bucket)
        .key(key)
        .content_type(content_type)
        .presigned(presigning_config)
        .await?;
    Ok(presigned.uri().to_string())
}

pub async fn presign_get_url(
    region: &str,
    endpoint_url: Option<&str>,
    creds: &S3StaticCredentials,
    bucket: &str,
    key: &str,
    expires_in: std::time::Duration,
) -> anyhow::Result<String> {
    let content = creds.to_credentials_content();
    let client = build_s3_client(region, Some(&content), None, endpoint_url).await?;
    let presigning_config = aws_sdk_s3::presigning::PresigningConfig::expires_in(expires_in)?;
    let presigned = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .presigned(presigning_config)
        .await?;
    Ok(presigned.uri().to_string())
}

/// Multipart upload for payloads that exceed [`MULTIPART_THRESHOLD`].
///
/// Splits `content` into [`MULTIPART_PART_SIZE`] chunks and uploads each as a
/// separate part via the S3 multipart upload API. Aborts the upload on any
/// part failure so we don't leak incomplete multipart uploads.
async fn multipart_upload_bytes(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    object_path: &str,
    content: &[u8],
    content_type: &str,
) -> anyhow::Result<()> {
    let create = client
        .create_multipart_upload()
        .bucket(bucket)
        .key(object_path)
        .content_type(content_type)
        .send()
        .await
        .with_context(|| {
            format!("Failed to create multipart upload for s3://{bucket}/{object_path}")
        })?;

    let upload_id = create
        .upload_id()
        .context("CreateMultipartUpload response missing upload_id")?
        .to_owned();

    let mut completed_parts = Vec::new();
    let mut offset = 0usize;
    let mut part_number = 1i32;

    let result: anyhow::Result<()> = async {
        while offset < content.len() {
            let end = (offset + MULTIPART_PART_SIZE).min(content.len());
            let chunk = &content[offset..end];

            let upload_part = client
                .upload_part()
                .bucket(bucket)
                .key(object_path)
                .upload_id(&upload_id)
                .part_number(part_number)
                .body(aws_sdk_s3::primitives::ByteStream::from(chunk.to_vec()))
                .send()
                .await
                .with_context(|| {
                    format!("Failed to upload part {part_number} for s3://{bucket}/{object_path}")
                })?;

            let etag = upload_part
                .e_tag()
                .context("UploadPart response missing ETag")?
                .to_owned();

            completed_parts.push(
                aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(part_number)
                    .e_tag(etag)
                    .build(),
            );

            offset = end;
            part_number += 1;
        }

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(completed_parts))
            .build();

        client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(object_path)
            .upload_id(&upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .with_context(|| {
                format!("Failed to complete multipart upload for s3://{bucket}/{object_path}")
            })?;

        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(object_path)
            .upload_id(&upload_id)
            .send()
            .await;
    }

    result
}

/// Upload bytes to an S3-compatible bucket.
pub async fn upload_bytes(
    bucket: &str,
    object_path: &str,
    content: &[u8],
    content_type: &str,
    region: &str,
    credentials_content: Option<&str>,
    credentials_file: Option<&str>,
    endpoint_url: Option<&str>,
) -> anyhow::Result<String> {
    let client =
        build_s3_client(region, credentials_content, credentials_file, endpoint_url).await?;
    if content.len() >= MULTIPART_THRESHOLD {
        multipart_upload_bytes(&client, bucket, object_path, content, content_type).await?;
    } else {
        client
            .put_object()
            .bucket(bucket)
            .key(object_path)
            .content_type(content_type)
            .body(aws_sdk_s3::primitives::ByteStream::from(content.to_vec()))
            .send()
            .await
            .with_context(|| format!("Failed to upload to s3://{bucket}/{object_path}"))?;
    }
    Ok(format!("s3://{bucket}/{object_path}"))
}

/// Upload a file to an S3-compatible bucket by streaming from disk.
pub async fn upload_file(
    bucket: &str,
    object_path: &str,
    file_path: &Path,
    content_type: &str,
    region: &str,
    credentials_content: Option<&str>,
    credentials_file: Option<&str>,
    endpoint_url: Option<&str>,
) -> anyhow::Result<String> {
    let client =
        build_s3_client(region, credentials_content, credentials_file, endpoint_url).await?;
    let file_size = tokio::fs::metadata(file_path)
        .await
        .map(|m| m.len() as usize)
        .unwrap_or(0);
    if file_size >= MULTIPART_THRESHOLD {
        let content = tokio::fs::read(file_path).await.with_context(|| {
            format!("Failed to read file for S3 upload: {}", file_path.display())
        })?;
        multipart_upload_bytes(&client, bucket, object_path, &content, content_type).await?;
    } else {
        let body = aws_sdk_s3::primitives::ByteStream::from_path(file_path)
            .await
            .with_context(|| {
                format!("Failed to open file for S3 upload: {}", file_path.display())
            })?;
        client
            .put_object()
            .bucket(bucket)
            .key(object_path)
            .content_type(content_type)
            .body(body)
            .send()
            .await
            .with_context(|| format!("Failed to upload to s3://{bucket}/{object_path}"))?;
    }
    Ok(format!("s3://{bucket}/{object_path}"))
}

/// Upload an async reader to S3.
///
/// Buffers the reader into memory before uploading because S3 PutObject
/// requires a known Content-Length. For the primary caller (zstd-compressed
/// dedup blobs from the upload queue), the compressed output is typically
/// small enough that buffering is acceptable.
pub async fn upload_stream<R: tokio::io::AsyncRead + Send + Sync + 'static>(
    bucket: &str,
    object_path: &str,
    reader: R,
    content_type: &str,
    region: &str,
    credentials_content: Option<&str>,
    credentials_file: Option<&str>,
    endpoint_url: Option<&str>,
) -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;

    let mut buf = Vec::new();
    tokio::pin!(reader);
    reader.read_to_end(&mut buf).await?;

    upload_bytes(
        bucket,
        object_path,
        &buf,
        content_type,
        region,
        credentials_content,
        credentials_file,
        endpoint_url,
    )
    .await
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // Used once the S3 storage backend is wired up.
pub struct S3ExistsResponse {
    pub bucket: String,
    pub path: String,
    pub size: i64,
}

/// S3-native storage client providing batch operations via concurrent SDK calls.
///
/// Caches the AWS SDK `Client` for the lifetime of the struct, avoiding the
/// per-call `build_s3_client()` overhead.
#[allow(dead_code)] // Used once the S3 storage backend is wired up.
pub struct S3StorageClient {
    client: aws_sdk_s3::Client,
    bucket: String,
}

#[allow(dead_code)] // Used once the S3 storage backend is wired up.
impl S3StorageClient {
    pub fn bucket_name(&self) -> &str {
        &self.bucket
    }

    pub async fn new(
        bucket: String,
        region: &str,
        credentials_content: Option<&str>,
        credentials_file: Option<&str>,
        endpoint_url: Option<&str>,
    ) -> anyhow::Result<Self> {
        let client =
            build_s3_client(region, credentials_content, credentials_file, endpoint_url).await?;
        tracing::debug!(bucket = %bucket, region, endpoint = ?endpoint_url, "S3StorageClient created");
        Ok(Self { client, bucket })
    }

    /// Check existence of multiple S3 objects via concurrent HeadObject calls.
    ///
    /// Aggregates per-key outcomes worst-first: 401/403 → `Unauthorized`,
    /// non-404 transient → `ProbeFailed`, all-404 → `NotFound`, else `Found`.
    pub async fn batch_check_exists<S: AsRef<str>>(
        &self,
        paths: &[S],
    ) -> crate::storage_client::ExistsResult<HashSet<String>> {
        use crate::storage_client::ExistsResult;
        let client = &self.client;
        let bucket = &*self.bucket;

        // Collect into owned strings up front: HeadObject requires owned keys
        // and this keeps closures HRTB-clean for callers passing both
        // `&[String]` and `&[&str]` from the same async fn.
        let owned_paths: Vec<String> = paths
            .iter()
            .map(|p| <S as AsRef<str>>::as_ref(p).to_string())
            .collect();
        let total = owned_paths.len();

        #[derive(Debug)]
        enum PathOutcome {
            Exists(String),
            NotFound,
            Unauthorized,
            ProbeFailed,
        }

        let outcomes: Vec<PathOutcome> = futures::stream::iter(owned_paths)
            .map(|path| async move {
                match client.head_object().bucket(bucket).key(&path).send().await {
                    Ok(_) => PathOutcome::Exists(path),
                    Err(e) => match classify_head_error(&e) {
                        HeadOutcome::NotFound => PathOutcome::NotFound,
                        HeadOutcome::Unauthorized => PathOutcome::Unauthorized,
                        HeadOutcome::ProbeFailed => PathOutcome::ProbeFailed,
                    },
                }
            })
            .buffer_unordered(32)
            .collect()
            .await;

        let mut found = HashSet::new();
        let mut not_found_count: usize = 0;
        let mut any_unauthorized = false;
        let mut any_transient = false;
        for o in outcomes {
            match o {
                PathOutcome::Exists(p) => {
                    found.insert(p);
                }
                PathOutcome::NotFound => not_found_count += 1,
                PathOutcome::Unauthorized => any_unauthorized = true,
                PathOutcome::ProbeFailed => any_transient = true,
            }
        }
        if any_unauthorized {
            ExistsResult::Unauthorized
        } else if any_transient {
            ExistsResult::ProbeFailed
        } else if total > 0 && not_found_count == total {
            // Symmetric with the proxy: all-404 batch surfaces as NotFound,
            // not Found(empty_set).
            ExistsResult::NotFound
        } else {
            ExistsResult::Found(found)
        }
    }

    /// Upload multiple small files via concurrent PutObject calls.
    ///
    /// Returns the proxy-compatible `BatchUploadResult` type directly so
    /// downstream result-handling code stays unchanged.
    pub async fn batch_upload(
        &self,
        files: Vec<(String, Vec<u8>, String)>,
    ) -> Option<Vec<echo_agent_chat_proxy_types::BatchUploadResult>> {
        let client = &self.client;
        let bucket = &*self.bucket;
        let results: Vec<echo_agent_chat_proxy_types::BatchUploadResult> = futures::stream::iter(
            files,
        )
        .map(|(path, content, content_type)| async move {
            let size = content.len() as i64;
            let bucket_owned = bucket.to_string();
            let upload_result = if content.len() >= MULTIPART_THRESHOLD {
                multipart_upload_bytes(client, bucket, &path, &content, &content_type).await
            } else {
                client
                    .put_object()
                    .bucket(bucket)
                    .key(&path)
                    .content_type(&content_type)
                    .body(aws_sdk_s3::primitives::ByteStream::from(content))
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!(e))
            };
            match upload_result {
                Ok(_) => echo_agent_chat_proxy_types::BatchUploadResult {
                    path,
                    bucket: Some(bucket_owned),
                    status: echo_agent_chat_proxy_types::BatchUploadStatus::Ok,
                    size: Some(size),
                    generation: None,
                    error: None,
                },
                Err(e) => {
                    let error_msg = format!("{:#}", e);
                    tracing::warn!(path = %path, error = %error_msg, "S3 batch upload item failed");
                    echo_agent_chat_proxy_types::BatchUploadResult {
                        path,
                        bucket: Some(bucket_owned),
                        status: echo_agent_chat_proxy_types::BatchUploadStatus::Error,
                        size: None,
                        generation: None,
                        error: Some(error_msg),
                    }
                }
            }
        })
        .buffer_unordered(16)
        .collect()
        .await;

        Some(results)
    }

    /// Check if a single object exists via HeadObject.
    pub async fn check_exists(&self, path: &str) -> Option<S3ExistsResponse> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(path)
            .send()
            .await
        {
            Ok(output) => Some(S3ExistsResponse {
                bucket: self.bucket.clone(),
                path: path.to_string(),
                size: output.content_length().unwrap_or(0),
            }),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::Path as AxumPath,
        http::StatusCode,
        response::IntoResponse,
        routing::{head, put},
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// In-progress multipart uploads: upload_id -> (key, parts: part_number -> bytes).
    type MultipartUploads = HashMap<String, (String, HashMap<i32, Vec<u8>>)>;

    /// Shared state for the mock S3 server.
    struct MockS3State {
        objects: RwLock<HashMap<String, Vec<u8>>>,
        multipart_uploads: RwLock<MultipartUploads>,
    }

    fn xml_response(status: u16, body: String) -> axum::response::Response {
        axum::http::Response::builder()
            .status(status)
            .header("content-type", "application/xml")
            .body(axum::body::Body::from(body))
            .unwrap()
            .into_response()
    }

    /// Mock S3 server with PUT/GET/HEAD/POST/DELETE and multipart upload support.
    fn mock_s3_router() -> (Router, Arc<MockS3State>) {
        let state = Arc::new(MockS3State {
            objects: RwLock::new(HashMap::new()),
            multipart_uploads: RwLock::new(HashMap::new()),
        });

        let s = state.clone();
        let head_handler = move |AxumPath((_, key)): AxumPath<(String, String)>| {
            let state = s.clone();
            async move {
                match state.objects.read().await.get(&key) {
                    Some(data) => (StatusCode::OK, [("content-length", data.len().to_string())])
                        .into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        };

        let s = state.clone();
        let put_handler = move |AxumPath((_, key)): AxumPath<(String, String)>,
                                query: axum::extract::Query<HashMap<String, String>>,
                                body: axum::body::Bytes| {
            let state = s.clone();
            async move {
                if let (Some(pn), Some(uid)) = (query.get("partNumber"), query.get("uploadId")) {
                    let part_num: i32 = pn.parse().unwrap_or(0);
                    let mut uploads = state.multipart_uploads.write().await;
                    if let Some((_, parts)) = uploads.get_mut(uid) {
                        parts.insert(part_num, body.to_vec());
                        return xml_response(200, "<UploadPartResult/>".into());
                    }
                    return StatusCode::NOT_FOUND.into_response();
                }
                if body.len() > 16 * 1024 * 1024 {
                    return (StatusCode::BAD_REQUEST, "chunk too big").into_response();
                }
                state.objects.write().await.insert(key, body.to_vec());
                StatusCode::OK.into_response()
            }
        };

        let s = state.clone();
        let get_handler = move |AxumPath((_, key)): AxumPath<(String, String)>| {
            let state = s.clone();
            async move {
                match state.objects.read().await.get(&key) {
                    Some(data) => (StatusCode::OK, data.clone()).into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        };

        let s = state.clone();
        let post_handler = move |AxumPath((_, key)): AxumPath<(String, String)>,
                                 query: axum::extract::Query<HashMap<String, String>>,
                                 body: axum::body::Bytes| {
            let state = s.clone();
            async move {
                if query.contains_key("uploads") {
                    use std::sync::atomic::{AtomicU64, Ordering};
                    static CTR: AtomicU64 = AtomicU64::new(0);
                    let uid = format!("upload-{}", CTR.fetch_add(1, Ordering::Relaxed));
                    state
                        .multipart_uploads
                        .write()
                        .await
                        .insert(uid.clone(), (key.clone(), HashMap::new()));
                    return xml_response(
                        200,
                        format!(
                            "<InitiateMultipartUploadResult><UploadId>{uid}</UploadId></InitiateMultipartUploadResult>"
                        ),
                    );
                }
                if let Some(uid) = query.get("uploadId") {
                    let mut uploads = state.multipart_uploads.write().await;
                    if let Some((stored_key, parts)) = uploads.remove(uid) {
                        let mut sorted: Vec<_> = parts.into_iter().collect();
                        sorted.sort_by_key(|(n, _)| *n);
                        let combined: Vec<u8> = sorted.into_iter().flat_map(|(_, d)| d).collect();
                        state.objects.write().await.insert(stored_key, combined);
                        return xml_response(200, "<CompleteMultipartUploadResult/>".into());
                    }
                    return StatusCode::NOT_FOUND.into_response();
                }
                let _ = body;
                StatusCode::BAD_REQUEST.into_response()
            }
        };

        let s = state.clone();
        let delete_handler =
            move |AxumPath((_, key)): AxumPath<(String, String)>,
                  query: axum::extract::Query<HashMap<String, String>>| {
                let state = s.clone();
                async move {
                    if let Some(uid) = query.get("uploadId") {
                        state.multipart_uploads.write().await.remove(uid);
                    } else {
                        state.objects.write().await.remove(&key);
                    }
                    StatusCode::NO_CONTENT
                }
            };

        let router = Router::new().route(
            "/{bucket}/{*key}",
            head(head_handler)
                .put(put_handler)
                .get(get_handler)
                .post(post_handler)
                .delete(delete_handler),
        );
        (router, state)
    }

    /// Start the mock server and return (endpoint_url, state).
    async fn start_mock_server() -> (String, Arc<MockS3State>) {
        let (router, state) = mock_s3_router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (format!("http://{addr}"), state)
    }

    async fn make_test_client(endpoint_url: &str) -> S3StorageClient {
        S3StorageClient::new(
            "test-bucket".to_string(),
            "us-east-1",
            Some(r#"{"aws_access_key_id":"test","aws_secret_access_key":"test"}"#),
            None,
            Some(endpoint_url),
        )
        .await
        .unwrap()
    }

    /// Start a mock server that rejects PUT for keys containing "fail".
    async fn start_mock_server_rejecting_fail_keys() -> String {
        let put_handler = move |AxumPath((_, key)): AxumPath<(String, String)>,
                                _body: axum::body::Bytes| async move {
            if key.contains("fail") {
                StatusCode::FORBIDDEN.into_response()
            } else {
                StatusCode::OK.into_response()
            }
        };

        let router = Router::new().route("/{bucket}/{*key}", put(put_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn unwrap_found(r: crate::storage_client::ExistsResult<HashSet<String>>) -> HashSet<String> {
        match r {
            crate::storage_client::ExistsResult::Found(s) => s,
            other => panic!("expected Found, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn batch_check_exists_returns_existing_paths() {
        let (endpoint, state) = start_mock_server().await;
        {
            let mut objects = state.objects.write().await;
            objects.insert("file-a.txt".into(), b"hello".to_vec());
            objects.insert("file-c.txt".into(), b"world".to_vec());
        }

        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec![
            "file-a.txt".into(),
            "file-b.txt".into(),
            "file-c.txt".into(),
        ];

        let result = unwrap_found(client.batch_check_exists(&paths).await);
        assert_eq!(result.len(), 2);
        assert!(result.contains("file-a.txt"));
        assert!(result.contains("file-c.txt"));
        assert!(!result.contains("file-b.txt"));
    }

    #[tokio::test]
    async fn batch_check_exists_empty_input() {
        let (endpoint, _state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;

        let paths: &[String] = &[];
        let result = unwrap_found(client.batch_check_exists(paths).await);
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn batch_check_exists_all_missing() {
        let (endpoint, _state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec!["missing-1".into(), "missing-2".into()];

        // All-404 collapses to top-level NotFound, symmetric with the proxy.
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, crate::storage_client::ExistsResult::NotFound),
            "all-404 batch must map to NotFound, got {:?}",
            result
        );
    }

    /// Build a HEAD-only mock router that returns `responder(key)` per key.
    async fn start_head_mock<F>(responder: F) -> String
    where
        F: Fn(String) -> axum::http::StatusCode + Clone + Send + Sync + 'static,
    {
        use axum::routing::head as axum_head;

        let h = move |AxumPath((_, key)): AxumPath<(String, String)>| {
            let responder = responder.clone();
            async move {
                let code = responder(key);
                code.into_response()
            }
        };
        let router = Router::new().route("/{bucket}/{*key}", axum_head(h));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn batch_check_exists_403_maps_to_unauthorized() {
        let endpoint = start_head_mock(|_| axum::http::StatusCode::FORBIDDEN).await;
        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec!["any.txt".into()];
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, crate::storage_client::ExistsResult::Unauthorized),
            "403 HEAD must map to Unauthorized, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn batch_check_exists_401_maps_to_unauthorized() {
        let endpoint = start_head_mock(|_| axum::http::StatusCode::UNAUTHORIZED).await;
        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec!["any.txt".into()];
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, crate::storage_client::ExistsResult::Unauthorized),
            "401 HEAD must map to Unauthorized, got {:?}",
            result
        );
    }

    /// Mixed `[Found, NotFound, 5xx]` → `ProbeFailed`; transient dominates.
    #[tokio::test]
    async fn batch_check_exists_mixed_with_5xx_maps_to_probe_failed() {
        let endpoint = start_head_mock(|key| {
            if key == "exists.txt" {
                axum::http::StatusCode::OK
            } else if key == "missing.txt" {
                axum::http::StatusCode::NOT_FOUND
            } else {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }
        })
        .await;
        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec!["exists.txt".into(), "missing.txt".into(), "boom.txt".into()];
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, crate::storage_client::ExistsResult::ProbeFailed),
            "any 5xx must dominate, got {:?}",
            result
        );
    }

    /// Mixed `[Found, 5xx, 401]` → `Unauthorized`; auth outranks transient.
    #[tokio::test]
    async fn batch_check_exists_mixed_with_401_maps_to_unauthorized() {
        let endpoint = start_head_mock(|key| {
            if key == "exists.txt" {
                axum::http::StatusCode::OK
            } else if key == "transient.txt" {
                axum::http::StatusCode::BAD_GATEWAY
            } else {
                axum::http::StatusCode::UNAUTHORIZED
            }
        })
        .await;
        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec![
            "exists.txt".into(),
            "transient.txt".into(),
            "no-auth.txt".into(),
        ];
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, crate::storage_client::ExistsResult::Unauthorized),
            "any 401 must dominate transient, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn batch_check_exists_transient_error_maps_to_probe_failed() {
        // Spin up a server whose HEAD returns 500 for everything.
        use axum::http::StatusCode;
        use axum::routing::head as axum_head;

        let head_handler = move |AxumPath((_, _key)): AxumPath<(String, String)>| async move {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        };
        let router = Router::new().route("/{bucket}/{*key}", axum_head(head_handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let endpoint = format!("http://{addr}");

        let client = make_test_client(&endpoint).await;
        let paths: Vec<String> = vec!["x".into()];
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, crate::storage_client::ExistsResult::ProbeFailed),
            "5xx HEAD must map to ProbeFailed, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn batch_upload_all_succeed() {
        let (endpoint, state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;

        let files = vec![
            (
                "upload-a.txt".into(),
                b"content-a".to_vec(),
                "text/plain".into(),
            ),
            (
                "upload-b.bin".into(),
                b"content-b".to_vec(),
                "application/octet-stream".into(),
            ),
        ];

        let results = client.batch_upload(files).await.unwrap();
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.status, echo_agent_chat_proxy_types::BatchUploadStatus::Ok);
            assert!(r.error.is_none());
            assert_eq!(r.bucket.as_deref(), Some("test-bucket"));
        }

        let objects = state.objects.read().await;
        assert_eq!(objects.get("upload-a.txt").unwrap(), b"content-a");
        assert_eq!(objects.get("upload-b.bin").unwrap(), b"content-b");
    }

    #[tokio::test]
    async fn batch_upload_reports_size() {
        let (endpoint, _state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;

        let content = b"twelve chars".to_vec();
        let expected_size = content.len() as i64;
        let files = vec![("sized.txt".into(), content, "text/plain".into())];

        let results = client.batch_upload(files).await.unwrap();
        assert_eq!(results[0].size, Some(expected_size));
    }

    #[tokio::test]
    async fn batch_upload_empty_input() {
        let (endpoint, _state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;

        let results = client.batch_upload(vec![]).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn check_exists_found() {
        let (endpoint, state) = start_mock_server().await;
        state
            .objects
            .write()
            .await
            .insert("existing.txt".into(), b"data123".to_vec());

        let client = make_test_client(&endpoint).await;
        let resp = client.check_exists("existing.txt").await.unwrap();
        assert_eq!(resp.bucket, "test-bucket");
        assert_eq!(resp.path, "existing.txt");
        assert_eq!(resp.size, 7);
    }

    #[tokio::test]
    async fn check_exists_not_found() {
        let (endpoint, _state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;

        assert!(client.check_exists("nonexistent.txt").await.is_none());
    }

    #[tokio::test]
    async fn batch_upload_then_batch_exists_roundtrip() {
        let (endpoint, _state) = start_mock_server().await;
        let client = make_test_client(&endpoint).await;

        let files = vec![
            ("rt-a.txt".into(), b"a".to_vec(), "text/plain".into()),
            ("rt-b.txt".into(), b"b".to_vec(), "text/plain".into()),
        ];
        client.batch_upload(files).await.unwrap();

        let paths: Vec<String> = vec!["rt-a.txt".into(), "rt-b.txt".into(), "rt-c.txt".into()];
        let existing = unwrap_found(client.batch_check_exists(&paths).await);
        assert_eq!(existing.len(), 2);
        assert!(existing.contains("rt-a.txt"));
        assert!(existing.contains("rt-b.txt"));
    }

    #[tokio::test]
    async fn batch_upload_partial_failure() {
        let endpoint = start_mock_server_rejecting_fail_keys().await;
        let client = make_test_client(&endpoint).await;

        let files = vec![
            ("good.txt".into(), b"ok".to_vec(), "text/plain".into()),
            ("fail-item.txt".into(), b"bad".to_vec(), "text/plain".into()),
            (
                "also-good.txt".into(),
                b"fine".to_vec(),
                "text/plain".into(),
            ),
        ];

        let results = client.batch_upload(files).await.unwrap();
        assert_eq!(results.len(), 3);

        let by_path: HashMap<&str, &echo_agent_chat_proxy_types::BatchUploadResult> =
            results.iter().map(|r| (r.path.as_str(), r)).collect();

        let good = by_path["good.txt"];
        assert_eq!(
            good.status,
            echo_agent_chat_proxy_types::BatchUploadStatus::Ok
        );
        assert!(good.size.is_some());
        assert!(good.error.is_none());

        let fail = by_path["fail-item.txt"];
        assert_eq!(
            fail.status,
            echo_agent_chat_proxy_types::BatchUploadStatus::Error
        );
        assert!(fail.size.is_none());
        assert!(fail.error.is_some());

        let also_good = by_path["also-good.txt"];
        assert_eq!(
            also_good.status,
            echo_agent_chat_proxy_types::BatchUploadStatus::Ok
        );
    }

    #[tokio::test]
    async fn new_returns_error_for_invalid_credentials_file() {
        let result = S3StorageClient::new(
            "test-bucket".to_string(),
            "us-east-1",
            None,
            Some("/nonexistent/path/to/credentials"),
            None,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn upload_stream_sends_content() {
        let (endpoint, state) = start_mock_server().await;
        let content = b"streamed content here";
        let reader = std::io::Cursor::new(content.to_vec());

        let result = upload_stream(
            "test-bucket",
            "stream-test.txt",
            reader,
            "text/plain",
            "us-east-1",
            Some(r#"{"aws_access_key_id":"test","aws_secret_access_key":"test"}"#),
            None,
            Some(&endpoint),
        )
        .await
        .unwrap();

        assert_eq!(result, "s3://test-bucket/stream-test.txt");
        let objects = state.objects.read().await;
        assert_eq!(objects.get("stream-test.txt").unwrap(), content);
    }

    #[tokio::test]
    async fn upload_stream_large_content() {
        let (endpoint, state) = start_mock_server().await;

        // 100 KB — larger payload exercising multi-chunk ReaderStream reads
        let content: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let reader = std::io::Cursor::new(content.clone());

        let result = upload_stream(
            "test-bucket",
            "large-stream.bin",
            reader,
            "application/octet-stream",
            "us-east-1",
            Some(r#"{"aws_access_key_id":"test","aws_secret_access_key":"test"}"#),
            None,
            Some(&endpoint),
        )
        .await
        .unwrap();

        assert_eq!(result, "s3://test-bucket/large-stream.bin");
        let objects = state.objects.read().await;
        assert_eq!(objects.get("large-stream.bin").unwrap(), &content);
    }

    #[test]
    fn multipart_threshold_is_under_16mib() {
        const MAX_CHUNK: usize = 16 * 1024 * 1024;
        const _: () = assert!(MULTIPART_THRESHOLD <= MAX_CHUNK);
        const _: () = assert!(MULTIPART_PART_SIZE <= MAX_CHUNK);
    }

    #[tokio::test]
    async fn upload_bytes_uses_single_put_for_small_content() {
        let (endpoint, state) = start_mock_server().await;

        let content = b"small payload".to_vec();
        upload_bytes(
            "test-bucket",
            "small-single.txt",
            &content,
            "text/plain",
            "us-east-1",
            Some(r#"{"aws_access_key_id":"test","aws_secret_access_key":"test"}"#),
            None,
            Some(&endpoint),
        )
        .await
        .unwrap();

        let objects = state.objects.read().await;
        assert_eq!(objects.get("small-single.txt").unwrap(), &content);
        assert!(state.multipart_uploads.read().await.is_empty());
    }

    /// Full multipart roundtrip against a real S3-compatible endpoint.
    #[tokio::test]
    #[ignore]
    async fn integration_multipart_upload_roundtrip() {
        let (endpoint, bucket, access_key, secret_key, region) = match integration_test_config() {
            Some(c) => c,
            None => return,
        };
        let creds = integration_creds(&access_key, &secret_key);
        let raw = make_raw_sdk_client(&endpoint, &access_key, &secret_key, &region).await;

        let prefix = unique_prefix();
        let key = format!("{prefix}/multipart-large.bin");

        // 20 MiB of patterned data, exceeds MULTIPART_THRESHOLD
        let content: Vec<u8> = (0..20 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

        let result = upload_bytes(
            &bucket,
            &key,
            &content,
            "application/octet-stream",
            &region,
            Some(&creds),
            None,
            Some(&endpoint),
        )
        .await
        .unwrap();

        assert_eq!(result, format!("s3://{bucket}/{key}"));

        let obj = raw
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .unwrap();
        let body = obj.body.collect().await.unwrap().into_bytes();
        assert_eq!(&body[..], &content[..]);
    }

    // --- Integration tests (require S3_TEST_ENDPOINT env var pointing at MinIO or similar) ---

    fn integration_test_config() -> Option<(String, String, String, String, String)> {
        Some((
            std::env::var("S3_TEST_ENDPOINT").ok()?,
            std::env::var("S3_TEST_BUCKET").ok()?,
            std::env::var("S3_TEST_ACCESS_KEY").ok()?,
            std::env::var("S3_TEST_SECRET_KEY").ok()?,
            std::env::var("S3_TEST_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
        ))
    }

    fn integration_creds(access_key: &str, secret_key: &str) -> String {
        serde_json::json!({
            "aws_access_key_id": access_key,
            "aws_secret_access_key": secret_key,
        })
        .to_string()
    }

    async fn make_integration_client(
        endpoint: &str,
        bucket: &str,
        access_key: &str,
        secret_key: &str,
        region: &str,
    ) -> S3StorageClient {
        let creds = integration_creds(access_key, secret_key);
        S3StorageClient::new(
            bucket.to_string(),
            region,
            Some(&creds),
            None,
            Some(endpoint),
        )
        .await
        .unwrap()
    }

    async fn make_raw_sdk_client(
        endpoint: &str,
        access_key: &str,
        secret_key: &str,
        region: &str,
    ) -> aws_sdk_s3::Client {
        let creds = integration_creds(access_key, secret_key);
        build_s3_client(region, Some(&creds), None, Some(endpoint))
            .await
            .unwrap()
    }

    fn unique_prefix() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("test-{nanos}-{}", std::process::id())
    }

    #[tokio::test]
    #[ignore]
    async fn integration_batch_upload_and_verify_content() {
        let (endpoint, bucket, access_key, secret_key, region) = match integration_test_config() {
            Some(c) => c,
            None => return,
        };
        let client =
            make_integration_client(&endpoint, &bucket, &access_key, &secret_key, &region).await;
        let raw = make_raw_sdk_client(&endpoint, &access_key, &secret_key, &region).await;

        let prefix = unique_prefix();
        let key_a = format!("{prefix}/a.txt");
        let key_b = format!("{prefix}/b.bin");

        let files = vec![
            (key_a.clone(), b"hello-a".to_vec(), "text/plain".to_string()),
            (
                key_b.clone(),
                b"\x00\x01\x02binary".to_vec(),
                "application/octet-stream".to_string(),
            ),
        ];

        let results = client.batch_upload(files).await.unwrap();
        assert_eq!(results.len(), 2);
        for r in &results {
            assert_eq!(r.status, echo_agent_chat_proxy_types::BatchUploadStatus::Ok);
        }

        let obj_a = raw
            .get_object()
            .bucket(&bucket)
            .key(&key_a)
            .send()
            .await
            .unwrap();
        let body_a = obj_a.body.collect().await.unwrap().into_bytes();
        assert_eq!(&body_a[..], b"hello-a");

        let obj_b = raw
            .get_object()
            .bucket(&bucket)
            .key(&key_b)
            .send()
            .await
            .unwrap();
        let body_b = obj_b.body.collect().await.unwrap().into_bytes();
        assert_eq!(&body_b[..], b"\x00\x01\x02binary");
    }

    #[tokio::test]
    #[ignore]
    async fn integration_batch_check_exists_partitions_correctly() {
        let (endpoint, bucket, access_key, secret_key, region) = match integration_test_config() {
            Some(c) => c,
            None => return,
        };
        let client =
            make_integration_client(&endpoint, &bucket, &access_key, &secret_key, &region).await;

        let prefix = unique_prefix();
        let uploaded_1 = format!("{prefix}/exists-1.txt");
        let uploaded_2 = format!("{prefix}/exists-2.txt");
        let missing = format!("{prefix}/does-not-exist.txt");

        let files = vec![
            (
                uploaded_1.clone(),
                b"data1".to_vec(),
                "text/plain".to_string(),
            ),
            (
                uploaded_2.clone(),
                b"data2".to_vec(),
                "text/plain".to_string(),
            ),
        ];
        client.batch_upload(files).await.unwrap();

        let paths = vec![uploaded_1.clone(), missing.clone(), uploaded_2.clone()];
        let existing = unwrap_found(client.batch_check_exists(&paths).await);
        assert_eq!(existing.len(), 2);
        assert!(existing.contains(&uploaded_1));
        assert!(existing.contains(&uploaded_2));
        assert!(!existing.contains(&missing));

        // All-missing query collapses to top-level NotFound (symmetric with proxy).
        let all_missing = vec![format!("{prefix}/nope-1"), format!("{prefix}/nope-2")];
        assert!(matches!(
            client.batch_check_exists(&all_missing).await,
            crate::storage_client::ExistsResult::NotFound
        ));
    }

    #[tokio::test]
    #[ignore]
    async fn integration_upload_stream_roundtrip() {
        let (endpoint, bucket, access_key, secret_key, region) = match integration_test_config() {
            Some(c) => c,
            None => return,
        };
        let creds = integration_creds(&access_key, &secret_key);
        let raw = make_raw_sdk_client(&endpoint, &access_key, &secret_key, &region).await;

        let prefix = unique_prefix();
        let key = format!("{prefix}/streamed.bin");

        // 50 KB of patterned data to exercise multi-chunk ReaderStream reads
        let content: Vec<u8> = (0..50_000).map(|i| (i % 251) as u8).collect();
        let reader = std::io::Cursor::new(content.clone());

        let result = upload_stream(
            &bucket,
            &key,
            reader,
            "application/octet-stream",
            &region,
            Some(&creds),
            None,
            Some(&endpoint),
        )
        .await
        .unwrap();

        assert_eq!(result, format!("s3://{bucket}/{key}"));

        let obj = raw
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .unwrap();
        let body = obj.body.collect().await.unwrap().into_bytes();
        assert_eq!(&body[..], &content[..]);
    }

    #[tokio::test]
    #[ignore]
    async fn integration_check_exists_single_roundtrip() {
        let (endpoint, bucket, access_key, secret_key, region) = match integration_test_config() {
            Some(c) => c,
            None => return,
        };
        let client =
            make_integration_client(&endpoint, &bucket, &access_key, &secret_key, &region).await;

        let prefix = unique_prefix();
        let key = format!("{prefix}/single.txt");

        assert!(client.check_exists(&key).await.is_none());

        client
            .batch_upload(vec![(
                key.clone(),
                b"present".to_vec(),
                "text/plain".to_string(),
            )])
            .await
            .unwrap();

        let resp = client.check_exists(&key).await.unwrap();
        assert_eq!(resp.path, key);
        assert_eq!(resp.bucket, bucket);
    }

    #[tokio::test]
    #[ignore]
    async fn integration_empty_file_upload() {
        let (endpoint, bucket, access_key, secret_key, region) = match integration_test_config() {
            Some(c) => c,
            None => return,
        };
        let client =
            make_integration_client(&endpoint, &bucket, &access_key, &secret_key, &region).await;
        let raw = make_raw_sdk_client(&endpoint, &access_key, &secret_key, &region).await;

        let prefix = unique_prefix();
        let key = format!("{prefix}/empty.bin");

        let results = client
            .batch_upload(vec![(
                key.clone(),
                vec![],
                "application/octet-stream".to_string(),
            )])
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].status,
            echo_agent_chat_proxy_types::BatchUploadStatus::Ok
        );
        assert_eq!(results[0].size, Some(0));

        let obj = raw
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .unwrap();
        let body = obj.body.collect().await.unwrap().into_bytes();
        assert!(body.is_empty());

        assert!(client.check_exists(&key).await.is_some());
    }
}
