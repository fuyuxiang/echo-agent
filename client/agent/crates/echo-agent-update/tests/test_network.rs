//! Network-level integration tests using `wiremock`.
//!
//! Covers the HTTP-fetching paths in `version.rs` that take a URL parameter
//! directly. We don't need `serial_test` here because each `MockServer` binds
//! to its own random port and tests don't touch global state.
//!
//! NOTE on retry timing: the prod retry backoff is 1s + 2s + 4s = 7s
//! wall-clock. We can't use `tokio::time::pause()` because reqwest's I/O
//! reactor uses the same tokio timer and stalls when time is paused. So
//! retry-exhaustion tests are intrinsically slow (~7s each); we keep the
//! count small and let them run in parallel (wiremock binds random ports
//! so there's no contention).

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use echo_agent_update::auto_update::{download_silent, download_with_progress};
use echo_agent_update::version::fetch_gcs_version_from_base;

// ─────────────────────────────────────────────────────────────────────────────
// Happy-path tests (fast, no retries triggered).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn gcs_pointer_returns_version_on_success() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181\n"))
        .expect(1)
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
async fn gcs_pointer_trims_whitespace() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("  0.1.181  \r\n  "))
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
async fn gcs_pointer_rejects_invalid_semver_no_retry() {
    // Invalid semver in the channel pointer is a hard error — must NOT
    // retry (it's a server data bug, not a transient failure).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not-a-version"))
        .expect(1) // exactly one request — no retry on parse failure
        .mount(&server)
        .await;

    let err = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("invalid semver"), "msg: {msg}");
}

#[tokio::test]
async fn gcs_pointer_alpha_channel_returns_max_of_alpha_and_stable_when_stable_higher() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.180-alpha.5"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .expect(1)
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("alpha", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
async fn gcs_pointer_alpha_returns_alpha_when_higher() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.182-alpha.1"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("alpha", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.182-alpha.1");
}

#[tokio::test]
async fn gcs_pointer_stable_channel_does_not_fetch_alpha() {
    // Stable-channel users should not pay the cost of fetching the alpha
    // pointer. The mock for /alpha should never be hit.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
async fn gcs_pointer_with_long_pre_release_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.190-alpha.42"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.189"))
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("alpha", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.190-alpha.42");
}

#[tokio::test]
async fn gcs_pointer_preserves_path_in_base_url() {
    // base_url may include a path component (in practice the prod GCS URL
    // does: `/cli`). The function appends `/{channel}`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cli/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&server)
        .await;

    let base = format!("{}/cli", server.uri());
    let v = fetch_gcs_version_from_base("stable", &base).await.unwrap();
    assert_eq!(v, "0.1.181");
}

// ─────────────────────────────────────────────────────────────────────────────
// Retry behavior — these tests intentionally exercise the 1s+2s+4s backoff,
// so each takes ~7 seconds. They run in parallel.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn gcs_pointer_retries_on_5xx_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(503).set_body_string("backend down"))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
async fn gcs_pointer_gives_up_after_max_retries() {
    let server = MockServer::start().await;
    // 4 attempts total: initial + 3 retries.
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(500))
        .expect(4)
        .mount(&server)
        .await;

    let err = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "msg: {msg}");
}

#[tokio::test]
async fn gcs_pointer_retries_on_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.181"))
        .mount(&server)
        .await;

    let v = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap();
    assert_eq!(v, "0.1.181");
}

#[tokio::test]
async fn gcs_pointer_alpha_propagates_error_from_either_pointer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alpha"))
        .respond_with(ResponseTemplate::new(200).set_body_string("0.1.182-alpha.1"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(500))
        .expect(4)
        .mount(&server)
        .await;

    let err = fetch_gcs_version_from_base("alpha", &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "msg: {msg}");
}

#[tokio::test]
async fn gcs_pointer_4xx_is_retryable_until_exhausted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(404))
        .expect(4)
        .mount(&server)
        .await;

    let err = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 404"), "msg: {msg}");
}

#[tokio::test]
async fn gcs_pointer_includes_url_in_error_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stable"))
        .respond_with(ResponseTemplate::new(500))
        .expect(4)
        .mount(&server)
        .await;

    let err = fetch_gcs_version_from_base("stable", &server.uri())
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("/stable"), "url should be in error: {msg}");
}

#[tokio::test]
async fn gcs_pointer_connection_refused_is_retried_and_returns_error() {
    // Bind a TcpListener to claim a port, then drop it so connections refuse.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}");

    let err = fetch_gcs_version_from_base("stable", &url)
        .await
        .unwrap_err();
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("fetch failed")
            || msg.contains("connection")
            || msg.contains("error sending request")
            || msg.contains("refused"),
        "expected network error message, got: {msg}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// download_silent — same body shape as download_with_progress but no
// progress bar to capture.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn download_silent_writes_body_to_dest() {
    let server = MockServer::start().await;
    let body = b"binary contents \x00\x01\x02".to_vec();
    Mock::given(method("GET"))
        .and(path("/echo-agent-0.1.181-macos-aarch64"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    let url = format!("{}/echo-agent-0.1.181-macos-aarch64", server.uri());
    download_silent(&url, &dest).await.unwrap();

    let written = std::fs::read(&dest).unwrap();
    assert_eq!(written, body);
}

#[tokio::test]
async fn download_silent_preserves_binary_bytes_unchanged() {
    // Verify that arbitrary binary content (including null bytes, high
    // bytes, control chars) round-trips intact.
    let server = MockServer::start().await;
    let body: Vec<u8> = (0u8..=255).cycle().take(10_000).collect();
    Mock::given(method("GET"))
        .and(path("/bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("bin");
    download_silent(&format!("{}/bin", server.uri()), &dest)
        .await
        .unwrap();

    let written = std::fs::read(&dest).unwrap();
    assert_eq!(written, body);
}

#[tokio::test]
async fn download_silent_atomically_renames_via_tmp_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bin"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    download_silent(&format!("{}/bin", server.uri()), &dest)
        .await
        .unwrap();

    // After successful download, only the final file should exist.
    assert!(dest.exists());
    assert!(
        !dest.with_extension("tmp").exists(),
        "tmp file must be renamed away on success"
    );
}

/// A downloaded artifact must be published already executable (the install
/// path execs it right after download).
#[cfg(unix)]
#[tokio::test]
async fn download_silent_publishes_executable() {
    use std::os::unix::fs::PermissionsExt;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/bin"))
        .respond_with(ResponseTemplate::new(200).set_body_string("#!/bin/sh\necho ok\n"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echo-agent-0.1.181-linux-x86_64");
    download_silent(&format!("{}/bin", server.uri()), &dest)
        .await
        .unwrap();

    let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
    assert_ne!(
        mode & 0o111,
        0,
        "downloaded artifact must be executable on publish (mode {mode:o})"
    );
}

#[tokio::test]
async fn download_silent_fails_on_4xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    let err = download_silent(&format!("{}/missing", server.uri()), &dest)
        .await
        .unwrap_err();

    let msg = format!("{err:#}");
    assert!(msg.contains("Download failed"), "msg: {msg}");
    assert!(msg.contains("404"), "msg: {msg}");
    assert!(!dest.exists(), "no file should be created on HTTP error");
}

#[tokio::test]
async fn download_silent_fails_on_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    let err = download_silent(&format!("{}/x", server.uri()), &dest)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("503"));
}

#[tokio::test]
async fn download_silent_overwrites_existing_dest() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("new content"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    std::fs::write(&dest, "old content").unwrap();

    download_silent(&format!("{}/x", server.uri()), &dest)
        .await
        .unwrap();

    let written = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(written, "new content");
}

#[tokio::test]
async fn download_silent_handles_empty_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(Vec::<u8>::new()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    download_silent(&format!("{}/x", server.uri()), &dest)
        .await
        .unwrap();

    assert!(dest.exists());
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 0);
}

#[tokio::test]
async fn download_silent_streams_large_body() {
    // 5 MB to verify streaming (file is written incrementally, not loaded
    // entirely in memory before write).
    let server = MockServer::start().await;
    let body = vec![0xAB_u8; 5 * 1024 * 1024];
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    download_silent(&format!("{}/big", server.uri()), &dest)
        .await
        .unwrap();

    let written = std::fs::read(&dest).unwrap();
    assert_eq!(written.len(), body.len());
    assert_eq!(written, body);
}

#[tokio::test]
async fn download_silent_to_nonexistent_parent_dir_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hi"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    // Parent directory does NOT exist — should fail at file create.
    let dest = tmp.path().join("missing-subdir").join("echoagent");
    let err = download_silent(&format!("{}/x", server.uri()), &dest)
        .await
        .unwrap_err();
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("no such file") || msg.contains("not found") || msg.contains("os error"),
        "expected fs error: {msg}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// download_with_progress — same contract; covers the spinner path
// (no Content-Length) and the progress-bar path (with Content-Length).
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn download_with_progress_writes_body_with_content_length() {
    // Wiremock sets Content-Length when set_body_bytes is used, so this
    // exercises the determinate-progress-bar path.
    let server = MockServer::start().await;
    let body = b"binary content".to_vec();
    Mock::given(method("GET"))
        .and(path("/echoagent"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    download_with_progress(&format!("{}/echoagent", server.uri()), &dest)
        .await
        .unwrap();

    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

#[tokio::test]
async fn download_with_progress_fails_on_http_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    let err = download_with_progress(&format!("{}/x", server.uri()), &dest)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("Download failed"), "msg: {msg}");
    assert!(msg.contains("500"), "msg: {msg}");
}

#[tokio::test]
async fn download_with_progress_atomic_rename() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echoagent");
    download_with_progress(&format!("{}/x", server.uri()), &dest)
        .await
        .unwrap();

    assert!(dest.exists());
    assert!(!dest.with_extension("tmp").exists());
}

// ─────────────────────────────────────────────────────────────────────────────
// Parallel byte-range path — exercises the HEAD + 206 Partial Content code path
// in download_silent / download_with_progress for files >= 16 MiB.
// ─────────────────────────────────────────────────────────────────────────────

/// Wiremock responder for `GET` that honors `Range: bytes=A-B` with `206`.
/// Without a Range header it returns the full body with `200`.
#[derive(Clone)]
struct RangeResponder {
    body: std::sync::Arc<Vec<u8>>,
}

impl Respond for RangeResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let total = self.body.len();
        let spec = request
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("bytes=").map(|x| x.to_string()));
        if let Some(spec) = spec
            && let Some((start_str, end_str)) = spec.split_once('-')
            && let (Ok(start), Ok(end)) = (start_str.parse::<usize>(), end_str.parse::<usize>())
        {
            let end = end.min(total - 1);
            if start <= end {
                let slice = self.body[start..=end].to_vec();
                return ResponseTemplate::new(206)
                    .insert_header("content-range", format!("bytes {start}-{end}/{total}"))
                    .set_body_bytes(slice);
            }
        }
        ResponseTemplate::new(200).set_body_bytes((*self.body).clone())
    }
}

#[tokio::test]
async fn download_silent_parallel_path_reassembles_bytes() {
    // 32 MiB body — clears the parallel threshold and yields 2 chunks
    // (size_mb / 16 = 2, clamped to [1, 8]), so this actually exercises
    // concurrent range fetches and the seek+write reassembly.
    let body: Vec<u8> = (0u32..(32 * 1024 * 1024 / 4))
        .flat_map(|n| n.to_le_bytes())
        .collect();
    assert_eq!(body.len(), 32 * 1024 * 1024);
    let arc = std::sync::Arc::new(body.clone());

    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/big"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", body.len().to_string())
                .insert_header("accept-ranges", "bytes"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(RangeResponder { body: arc })
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("echo-agent-binary");
    download_silent(&format!("{}/big", server.uri()), &dest)
        .await
        .unwrap();

    let written = std::fs::read(&dest).unwrap();
    assert_eq!(written.len(), body.len());
    assert_eq!(
        written, body,
        "reassembled file must match original byte-for-byte"
    );
    assert!(
        !dest.with_extension("tmp").exists(),
        "tmp file must be cleaned up"
    );
}
