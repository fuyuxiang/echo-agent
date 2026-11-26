//! Signed upload URL types shared between cli-chat-proxy (server) and echo-agent-shell (client).

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct BatchExistsRequest {
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BatchExistsResponse {
    pub exists: Vec<String>,
    pub missing: Vec<String>,
}

/// Response from the signed upload URL endpoint.
/// `POST /v1/storage/signed-upload-url`
///
/// The client uses the returned `signed_url` to PUT the object directly to GCS,
/// completely bypassing the proxy for the data transfer.  This avoids nginx /
/// Cloudflare body-size limits that would otherwise cause 413 errors on large
/// payloads (e.g. session share data).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedUploadUrlResponse {
    /// Pre-signed GCS PUT URL. Upload the object body here with a simple PUT.
    pub signed_url: String,
    /// GCS bucket where the object will be stored.
    pub bucket: String,
    /// Object path within the bucket.
    pub path: String,
    /// Content-Type that was baked into the signed URL.
    /// The PUT request **must** use this exact Content-Type header.
    pub content_type: String,
    /// Validity window in seconds.
    pub expires_in_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BatchUploadResult {
    pub path: String,
    pub status: BatchUploadStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchUploadStatus {
    Ok,
    Error,
    Skipped,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BatchUploadResponse {
    pub results: Vec<BatchUploadResult>,
}

/// JSON request body for `POST /v1/storage/batch_upload_json`.
///
/// Each file's content is base64-encoded. The request is typically sent with
/// `Content-Encoding: zstd` so the JSON body is compressed on the wire.
#[derive(Debug, Deserialize, Serialize)]
pub struct BatchUploadRequest {
    pub files: Vec<BatchUploadFile>,
}

/// A single file entry in a [`BatchUploadRequest`].
///
/// All three fields are required on the wire. The server treats an empty
/// `content_type` as `"application/octet-stream"`, but the field itself
/// must be present in the JSON object.
#[derive(Debug, Deserialize, Serialize)]
pub struct BatchUploadFile {
    /// GCS destination path.
    pub path: String,
    /// MIME type of the file content. Required on the wire; the server
    /// defaults empty values to `"application/octet-stream"`.
    pub content_type: String,
    /// Base64-encoded file content (standard alphabet, with padding).
    pub data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_upload_status_serde_round_trip() {
        for (variant, expected_json) in [
            (BatchUploadStatus::Ok, "\"ok\""),
            (BatchUploadStatus::Error, "\"error\""),
            (BatchUploadStatus::Skipped, "\"skipped\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let deserialized: BatchUploadStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, variant);
        }
    }

    #[test]
    fn batch_upload_response_serializes_ok_result_with_metadata() {
        let resp = BatchUploadResponse {
            results: vec![BatchUploadResult {
                path: "data/file.txt".to_string(),
                status: BatchUploadStatus::Ok,
                bucket: Some("my-bucket".to_string()),
                size: Some(1024),
                generation: Some(42),
                error: None,
            }],
        };
        let json: serde_json::Value = serde_json::to_value(&resp).unwrap();
        let result = &json["results"][0];
        assert_eq!(result["path"], "data/file.txt");
        assert_eq!(result["status"], "ok");
        assert_eq!(result["bucket"], "my-bucket");
        assert_eq!(result["size"], 1024);
        assert_eq!(result["generation"], 42);
        assert!(result.get("error").is_none(), "None fields must be omitted");
    }

    #[test]
    fn batch_upload_response_serializes_error_result_without_metadata() {
        let resp = BatchUploadResponse {
            results: vec![BatchUploadResult {
                path: "data/fail.txt".to_string(),
                status: BatchUploadStatus::Error,
                bucket: None,
                size: None,
                generation: None,
                error: Some("upload failed".to_string()),
            }],
        };
        let json: serde_json::Value = serde_json::to_value(&resp).unwrap();
        let result = &json["results"][0];
        assert_eq!(result["status"], "error");
        assert_eq!(result["error"], "upload failed");
        assert!(result.get("bucket").is_none());
        assert!(result.get("size").is_none());
    }

    #[test]
    fn batch_upload_response_round_trips_mixed_results() {
        let original = BatchUploadResponse {
            results: vec![
                BatchUploadResult {
                    path: "ok.bin".to_string(),
                    status: BatchUploadStatus::Ok,
                    bucket: Some("b".to_string()),
                    size: Some(100),
                    generation: Some(1),
                    error: None,
                },
                BatchUploadResult {
                    path: "err.bin".to_string(),
                    status: BatchUploadStatus::Error,
                    bucket: None,
                    size: None,
                    generation: None,
                    error: Some("boom".to_string()),
                },
                BatchUploadResult {
                    path: "skip.bin".to_string(),
                    status: BatchUploadStatus::Skipped,
                    bucket: Some("b".to_string()),
                    size: Some(200),
                    generation: Some(5),
                    error: None,
                },
            ],
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: BatchUploadResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.results.len(), 3);
        assert_eq!(deserialized.results[0].status, BatchUploadStatus::Ok);
        assert_eq!(deserialized.results[1].status, BatchUploadStatus::Error);
        assert_eq!(deserialized.results[1].error.as_deref(), Some("boom"));
        assert_eq!(deserialized.results[2].status, BatchUploadStatus::Skipped);
        assert_eq!(deserialized.results[2].size, Some(200));
    }

    #[test]
    fn batch_upload_request_serializes_round_trip() {
        let req = BatchUploadRequest {
            files: vec![
                BatchUploadFile {
                    path: "a.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    data: "SGVsbG8=".to_string(), // "Hello" in base64
                },
                BatchUploadFile {
                    path: "b.bin".to_string(),
                    content_type: "application/octet-stream".to_string(),
                    data: "AAEC/w==".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: BatchUploadRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.files.len(), 2);
        assert_eq!(deserialized.files[0].path, "a.txt");
        assert_eq!(deserialized.files[0].data, "SGVsbG8=");
        assert_eq!(deserialized.files[1].path, "b.bin");
        assert_eq!(
            deserialized.files[1].content_type,
            "application/octet-stream"
        );
    }

    #[test]
    fn batch_upload_request_empty_files_round_trip() {
        let req = BatchUploadRequest { files: vec![] };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: BatchUploadRequest = serde_json::from_str(&json).unwrap();
        assert!(parsed.files.is_empty());
    }
}
