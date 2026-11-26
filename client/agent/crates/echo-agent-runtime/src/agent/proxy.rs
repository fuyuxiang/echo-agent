//! HTTP CONNECT proxy support for WebSocket connections.
//!
//! When running behind a corporate egress proxy,
//! `tokio-tungstenite`'s `connect_async` cannot reach external
//! hosts directly because it does not read the standard `HTTPS_PROXY` /
//! `HTTP_PROXY` environment variables.
//!
//! This module provides:
//! - [`resolve_proxy_for_host`]: reads proxy env vars and `NO_PROXY`, returning
//!   the proxy URL to use for a given target host (or `None` for direct).
//! - [`connect_via_proxy`]: opens a TCP connection to the proxy, sends an HTTP
//!   CONNECT request to create a tunnel, wraps the result in TLS, and returns a
//!   stream suitable for `tokio_tungstenite::client_async`.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_tungstenite::MaybeTlsStream;
use tracing::debug;

// ---------------------------------------------------------------------------
// Environment-variable resolution
// ---------------------------------------------------------------------------

/// Read proxy configuration from the environment and decide whether `target_host`
/// should be connected through a proxy.
///
/// Resolution order (matches `curl` / `reqwest` behaviour):
/// 1. If `NO_PROXY` contains `target_host` (or a matching domain suffix / CIDR),
///    return `None`.
/// 2. If `HTTPS_PROXY` (or `https_proxy`) is set, return its value.
/// 3. If `HTTP_PROXY` (or `http_proxy`) is set, return its value.
/// 4. Otherwise return `None`.
pub(crate) fn resolve_proxy_for_host(target_host: &str) -> Option<String> {
    resolve_proxy_for_host_with(target_host, |key| std::env::var(key))
}

/// Testable inner implementation that accepts a custom env-var reader.
fn resolve_proxy_for_host_with<F>(target_host: &str, env: F) -> Option<String>
where
    F: for<'a> Fn(&'a str) -> Result<String, std::env::VarError>,
{
    // Check NO_PROXY / no_proxy.
    let no_proxy = env("NO_PROXY")
        .or_else(|_| env("no_proxy"))
        .unwrap_or_default();
    if is_host_bypassed(target_host, &no_proxy) {
        return None;
    }

    // HTTPS_PROXY takes precedence (our target is always wss://).
    if let Ok(url) = env("HTTPS_PROXY").or_else(|_| env("https_proxy")) {
        let url = url.trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }

    // Fall back to HTTP_PROXY.
    if let Ok(url) = env("HTTP_PROXY").or_else(|_| env("http_proxy")) {
        let url = url.trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }

    None
}

/// Check whether `host` is in the `no_proxy` list.
///
/// The `no_proxy` value is a comma-separated list of hostnames, domain
/// suffixes (with or without a leading dot), IP addresses, or CIDR ranges.
/// The special value `*` matches everything.
fn is_host_bypassed(host: &str, no_proxy: &str) -> bool {
    let host_lower = host.to_ascii_lowercase();
    for entry in no_proxy.split(',') {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            continue;
        }
        // Wildcard — bypass all hosts.
        if entry == "*" {
            return true;
        }
        // Exact match.
        if host_lower == entry {
            return true;
        }
        // Domain suffix match: ".example.com" matches "foo.example.com".
        // Also handle the common convention of omitting the leading dot:
        // "example.com" in NO_PROXY should match "sub.example.com".
        let matches_suffix = if entry.starts_with('.') {
            host_lower.ends_with(entry.as_str())
        } else {
            host_lower.len() > entry.len()
                && host_lower.ends_with(entry.as_str())
                && host_lower.as_bytes()[host_lower.len() - entry.len() - 1] == b'.'
        };
        if matches_suffix {
            return true;
        }
        // CIDR / IP matching is intentionally omitted here — our target host
        // is always a DNS name, not an IP literal. Keeping this simple avoids
        // pulling in a CIDR parsing dependency.
    }
    false
}

// ---------------------------------------------------------------------------
// HTTP CONNECT tunnel
// ---------------------------------------------------------------------------

/// Establish a TLS-wrapped TCP stream through an HTTP CONNECT proxy.
///
/// Steps:
/// 1. Parse the proxy URL to get host + port.
/// 2. Open a TCP connection to the proxy and perform the CONNECT handshake.
/// 3. Wrap the tunnel in TLS (using rustls with native root certificates).
/// 4. Return the stream as `MaybeTlsStream<TcpStream>` so it is compatible
///    with `tokio_tungstenite::client_async`.
pub(crate) async fn connect_via_proxy(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
) -> anyhow::Result<MaybeTlsStream<TcpStream>> {
    let stream = open_connect_tunnel(proxy_url, target_host, target_port).await?;
    let tls_stream = tls_wrap(stream, target_host).await?;
    Ok(MaybeTlsStream::Rustls(tls_stream))
}

/// Open a raw TCP tunnel through an HTTP CONNECT proxy (no TLS).
///
/// 1. Parse the proxy URL to get host + port.
/// 2. Open a plain TCP connection to the proxy.
/// 3. Send `CONNECT target_host:target_port HTTP/1.1\r\n\r\n`.
/// 4. Read the proxy's response; expect `HTTP/1.x 200 …`.
/// 5. Return the raw `TcpStream` positioned after the CONNECT response.
async fn open_connect_tunnel(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
) -> anyhow::Result<TcpStream> {
    // 1. Parse proxy URL.
    let (proxy_host, proxy_port) = parse_proxy_url(proxy_url)?;

    // 2. TCP connect to proxy.
    let proxy_addr = format!("{proxy_host}:{proxy_port}");
    debug!(proxy_addr = %proxy_addr, "Opening TCP to proxy");
    let stream = TcpStream::connect(&proxy_addr)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to proxy at {proxy_addr}: {e}"))?;

    // 3. Send HTTP CONNECT.
    let connect_req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
         Host: {target_host}:{target_port}\r\n\
         \r\n"
    );
    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(connect_req.as_bytes()).await?;
    writer_half.flush().await?;

    // 4. Read the status line from the proxy.
    let mut reader = BufReader::new(reader_half);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).await?;
    debug!(status_line = %status_line.trim(), "Proxy CONNECT response");

    if !status_line.starts_with("HTTP/1.1 200") && !status_line.starts_with("HTTP/1.0 200") {
        anyhow::bail!("Proxy CONNECT failed: {}", status_line.trim());
    }

    // Consume remaining response headers (until empty line).
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    // 5. Assert the BufReader's internal buffer is empty before reuniting.
    // BufReader::read_line may have read ahead into its buffer. If extra
    // bytes were consumed beyond the HTTP headers (e.g., from a proxy that
    // eagerly forwards data or coalesced TCP segments), dropping them would
    // corrupt the subsequent TLS handshake.
    let remaining = reader.buffer();
    if !remaining.is_empty() {
        anyhow::bail!(
            "Proxy sent {} unexpected byte(s) after CONNECT response headers",
            remaining.len()
        );
    }

    // 6. Reunite the split halves back into a TcpStream.
    let stream = reader.into_inner().reunite(writer_half)?;
    Ok(stream)
}

async fn tls_wrap(
    stream: TcpStream,
    server_name: &str,
) -> anyhow::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let connector = tokio_rustls::TlsConnector::from(echo_agent_extra_ca::rustls_client_config());
    let dns_name = rustls::pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|e| anyhow::anyhow!("Invalid TLS server name '{server_name}': {e}"))?;

    let tls_stream = connector
        .connect(dns_name, stream)
        .await
        .map_err(|e| anyhow::anyhow!("TLS handshake through proxy failed: {e}"))?;

    Ok(tls_stream)
}

/// Parse a proxy URL into (host, port).
///
/// Accepted formats:
/// - `http://host:port`
/// - `http://host` (defaults to port 80)
/// - `host:port`
fn parse_proxy_url(url: &str) -> anyhow::Result<(String, u16)> {
    // Strip scheme if present.
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    // Strip trailing path/slash.
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);

    if let Some((host, port_str)) = authority.rsplit_once(':') {
        let port: u16 = port_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid proxy port in '{url}'"))?;
        Ok((host.to_string(), port))
    } else {
        // No port — default to 80 for HTTP proxies.
        Ok((authority.to_string(), 80))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ===== parse_proxy_url =====

    #[test]
    fn test_parse_proxy_url_with_scheme_and_port() {
        let (host, port) = parse_proxy_url("http://proxy.example.com:3140").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 3140);
    }

    #[test]
    fn test_parse_proxy_url_without_scheme() {
        let (host, port) = parse_proxy_url("proxy.example.com:8080").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_parse_proxy_url_without_port() {
        let (host, port) = parse_proxy_url("http://proxy.example.com").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 80);
    }

    #[test]
    fn test_parse_proxy_url_with_trailing_slash() {
        let (host, port) = parse_proxy_url("http://proxy.example.com:3140/").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 3140);
    }

    #[test]
    fn test_parse_proxy_url_https_scheme() {
        let (host, port) = parse_proxy_url("https://secure-proxy:443").unwrap();
        assert_eq!(host, "secure-proxy");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_parse_proxy_url_multi_label_host() {
        let (host, port) =
            parse_proxy_url("http://http-proxy.services.internal.example:3128").unwrap();
        assert_eq!(host, "http-proxy.services.internal.example");
        assert_eq!(port, 3128);
    }

    #[test]
    fn test_parse_proxy_url_invalid_port() {
        assert!(parse_proxy_url("http://proxy:notaport").is_err());
    }

    // ===== is_host_bypassed =====

    #[test]
    fn test_bypass_exact_match() {
        assert!(is_host_bypassed("localhost", "localhost,127.0.0.1"));
    }

    #[test]
    fn test_bypass_domain_suffix_with_dot() {
        assert!(is_host_bypassed(
            "api.corp.example",
            "localhost,.corp.example"
        ));
    }

    #[test]
    fn test_bypass_domain_suffix_without_dot() {
        // Common convention: "example.com" in NO_PROXY matches "api.example.com".
        assert!(is_host_bypassed("api.example.com", "localhost,example.com"));
    }

    #[test]
    fn test_bypass_wildcard() {
        assert!(is_host_bypassed("anything.example.com", "*"));
    }

    #[test]
    fn test_no_bypass_when_not_listed() {
        assert!(!is_host_bypassed(
            "api.external.example",
            "localhost,127.0.0.1,.corp.example,.internal.example"
        ));
    }

    #[test]
    fn test_bypass_case_insensitive() {
        assert!(is_host_bypassed("API.Corp.EXAMPLE", ".corp.example"));
    }

    #[test]
    fn test_bypass_empty_no_proxy() {
        assert!(!is_host_bypassed("api.external.example", ""));
    }

    #[test]
    fn test_bypass_spaces_in_entries() {
        assert!(is_host_bypassed(
            "foo.example.com",
            " localhost , .example.com , .other.com "
        ));
    }

    #[test]
    fn test_bypass_cidr_not_matched_for_dns_names() {
        // CIDR entries like 10.0.0.0/8 should not match DNS names.
        assert!(!is_host_bypassed("api.external.example", "10.0.0.0/8"));
    }

    #[test]
    fn test_bypass_combined_no_proxy_list() {
        // A typical corporate NO_PROXY mixes loopback, private CIDRs, and domain suffixes.
        let no_proxy = "localhost,127.0.0.1,10.0.0.0/8,.internal.example,.corp.example";
        assert!(!is_host_bypassed("api.external.example", no_proxy));
        assert!(is_host_bypassed("db.internal.example", no_proxy));
        assert!(is_host_bypassed("git.corp.example", no_proxy));
        assert!(is_host_bypassed("localhost", no_proxy));
    }

    // ===== resolve_proxy_for_host_with =====

    #[test]
    fn test_resolve_no_proxy_vars_set() {
        let result = resolve_proxy_for_host_with("api.external.example", |_| {
            Err(std::env::VarError::NotPresent)
        });
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_https_proxy_used() {
        let result = resolve_proxy_for_host_with("api.external.example", |key| match key {
            "HTTPS_PROXY" => Ok("http://proxy.example.com:3128".to_string()),
            "NO_PROXY" => Err(std::env::VarError::NotPresent),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(result, Some("http://proxy.example.com:3128".to_string()));
    }

    #[test]
    fn test_resolve_http_proxy_fallback() {
        let result = resolve_proxy_for_host_with("api.external.example", |key| match key {
            "HTTP_PROXY" => Ok("http://proxy.example.com:8080".to_string()),
            "NO_PROXY" => Err(std::env::VarError::NotPresent),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(result, Some("http://proxy.example.com:8080".to_string()));
    }

    #[test]
    fn test_resolve_no_proxy_bypasses() {
        let result = resolve_proxy_for_host_with("api.corp.example", |key| match key {
            "HTTPS_PROXY" => Ok("http://proxy.example.com:3128".to_string()),
            "NO_PROXY" => Ok("localhost,.corp.example".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_https_proxy_takes_precedence() {
        let result = resolve_proxy_for_host_with("api.external.example", |key| match key {
            "HTTPS_PROXY" => Ok("http://https-proxy.example.com:443".to_string()),
            "HTTP_PROXY" => Ok("http://http-proxy.example.com:80".to_string()),
            "NO_PROXY" => Err(std::env::VarError::NotPresent),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(
            result,
            Some("http://https-proxy.example.com:443".to_string())
        );
    }

    #[test]
    fn test_resolve_lowercase_env_vars() {
        let result = resolve_proxy_for_host_with("api.external.example", |key| match key {
            "https_proxy" => Ok("http://proxy.example.com:3128".to_string()),
            "no_proxy" => Err(std::env::VarError::NotPresent),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(result, Some("http://proxy.example.com:3128".to_string()));
    }

    #[test]
    fn test_resolve_empty_proxy_ignored() {
        let result = resolve_proxy_for_host_with("api.external.example", |key| match key {
            "HTTPS_PROXY" => Ok("  ".to_string()),
            "HTTP_PROXY" => Ok("http://proxy.example.com:8080".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(result, Some("http://proxy.example.com:8080".to_string()));
    }

    #[test]
    fn test_resolve_respects_no_proxy_when_proxy_set() {
        let result = resolve_proxy_for_host_with("api.external.example", |key| match key {
            "HTTPS_PROXY" | "HTTP_PROXY" => Ok("http://proxy.example.com:3128".to_string()),
            "NO_PROXY" => {
                Ok("localhost,127.0.0.1,10.0.0.0/8,.internal.example,.corp.example".to_string())
            }
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(result, Some("http://proxy.example.com:3128".to_string()));
    }

    // ===== HTTP CONNECT tunnel (integration-style) =====

    /// Helper: spawn a mock HTTP CONNECT proxy that accepts one connection.
    ///
    /// On receiving a CONNECT request, it validates the request format,
    /// replies with `status_line`, and then echoes data (simulating a tunnel).
    /// Returns the proxy's listen address.
    async fn spawn_mock_proxy(status_line: &'static str) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read CONNECT request (read until \r\n\r\n).
            let mut buf = vec![0u8; 4096];
            let mut total = 0;
            loop {
                let n = stream.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    return;
                }
                total += n;
                let so_far = std::str::from_utf8(&buf[..total]).unwrap_or("");
                if so_far.contains("\r\n\r\n") {
                    break;
                }
            }

            let request = std::str::from_utf8(&buf[..total]).unwrap().to_string();
            assert!(
                request.contains("CONNECT ") && request.contains(" HTTP/1.1"),
                "Expected CONNECT request, got: {request}"
            );

            // Reply with the provided status line.
            stream.write_all(status_line.as_bytes()).await.unwrap();

            // Echo loop (simulates the transparent tunnel).
            let mut echo_buf = [0u8; 1024];
            loop {
                let n = match stream.read(&mut echo_buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if stream.write_all(&echo_buf[..n]).await.is_err() {
                    break;
                }
            }
        });

        addr
    }

    /// Tests that `open_connect_tunnel` sends a correct CONNECT request,
    /// parses the proxy's 200 response, and returns a usable tunnel stream.
    #[tokio::test]
    async fn test_open_connect_tunnel_success() {
        let addr =
            spawn_mock_proxy("HTTP/1.1 200 Connection Established\r\nServer: mock\r\n\r\n").await;
        let proxy_url = format!("http://{addr}");

        // Call the real function under test.
        let mut stream = open_connect_tunnel(&proxy_url, "example.com", 443)
            .await
            .expect("tunnel should succeed");

        // Verify the tunnel works by echoing data through it.
        stream.write_all(b"hello tunnel").await.unwrap();
        stream.flush().await.unwrap();

        let mut response = vec![0u8; 12];
        stream.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"hello tunnel");
    }

    /// Tests that `open_connect_tunnel` with a non-default port sends the
    /// correct CONNECT target.
    #[tokio::test]
    async fn test_open_connect_tunnel_custom_port() {
        let addr = spawn_mock_proxy("HTTP/1.1 200 OK\r\n\r\n").await;
        let proxy_url = format!("http://{addr}");

        let stream = open_connect_tunnel(&proxy_url, "internal.example.com", 8443).await;
        assert!(stream.is_ok(), "tunnel should succeed for custom port");
    }

    /// Tests that `open_connect_tunnel` returns an error when the proxy
    /// rejects the CONNECT request with a non-200 status.
    #[tokio::test]
    async fn test_open_connect_tunnel_proxy_rejects() {
        let addr = spawn_mock_proxy("HTTP/1.1 403 Forbidden\r\n\r\n").await;
        let proxy_url = format!("http://{addr}");

        let result = open_connect_tunnel(&proxy_url, "blocked.example.com", 443).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("403"),
            "Error should mention 403: {err_msg}"
        );
    }

    /// Tests that `open_connect_tunnel` returns an error when connecting
    /// to a proxy that isn't listening.
    #[tokio::test]
    async fn test_open_connect_tunnel_proxy_unreachable() {
        let result = open_connect_tunnel("http://127.0.0.1:1", "example.com", 443).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to connect to proxy"),
            "Error should mention proxy connection failure: {err_msg}"
        );
    }
}
