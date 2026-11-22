//! A task-local forward proxy used by the managed browser.
//!
//! Checking only `Page.navigate` is not a network boundary: page scripts,
//! iframes, images, WebSockets and redirects can all contact other hosts. The
//! browser is therefore forced through this proxy, which resolves every
//! destination and applies the private-network policy before connecting. The
//! checked IP addresses are also pinned for that connection to close the usual
//! DNS-rebinding gap between validation and use.

use axum::body::Body;
use hyper::{
    header::{
        CONNECTION, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING,
        UPGRADE,
    },
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use std::{
    collections::HashSet,
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::copy_bidirectional,
    net::{lookup_host, TcpListener, TcpStream},
    sync::{oneshot, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(any(target_os = "macos", windows))]
mod interface_dns;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct BrowserNetworkProxy {
    address: SocketAddr,
    allow_private_network: Arc<AtomicBool>,
    development: DevelopmentEndpoints,
    connection_cancellation: Arc<Mutex<CancellationToken>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

type DevelopmentEndpoints = Arc<Mutex<HashSet<(String, u16)>>>;

fn development_endpoint(host: &str, port: u16) -> Option<(String, u16)> {
    let host = host.trim_matches(['[', ']']);
    matches!(host, "localhost" | "127.0.0.1" | "::1").then(|| (host.to_owned(), port))
}

pub(super) fn development_url_endpoint(url: &Url) -> Option<(String, u16)> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    development_endpoint(url.host_str()?, url.port_or_known_default()?)
}

async fn resolve_browser_destinations(
    host: &str,
    port: u16,
    allow_private: bool,
    development: &DevelopmentEndpoints,
) -> Result<Vec<SocketAddr>, String> {
    let approved = development_endpoint(host, port)
        .is_some_and(|endpoint| allow_private || development.lock().unwrap().contains(&endpoint));
    let host = host.trim_matches(['[', ']']);
    if !approved {
        return resolve_allowed_destinations(host, port, allow_private, true).await;
    }
    let addresses: Vec<_> = tokio::time::timeout(CONNECT_TIMEOUT, lookup_host((host, port)))
        .await
        .map_err(|_| "解析本地开发服务超时")?
        .map_err(|error| format!("无法解析本地开发服务：{error}"))?
        .filter(|address| address.ip().is_loopback())
        .collect();
    if addresses.is_empty() {
        return Err("开发服务地址必须解析到本机".into());
    }
    Ok(addresses)
}

impl std::fmt::Debug for BrowserNetworkProxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserNetworkProxy")
            .field("address", &self.address)
            .field(
                "allow_private_network",
                &self.allow_private_network.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

impl BrowserNetworkProxy {
    pub async fn start(allow_private_network: bool) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| format!("创建浏览器网络隔离代理失败：{error}"))?;
        let address = listener
            .local_addr()
            .map_err(|error| format!("读取浏览器代理地址失败：{error}"))?;
        let policy = Arc::new(AtomicBool::new(allow_private_network));
        let development = DevelopmentEndpoints::default();
        let connection_cancellation = Arc::new(Mutex::new(CancellationToken::new()));
        let task_policy = Arc::clone(&policy);
        let task_development = Arc::clone(&development);
        let task_cancellation = Arc::clone(&connection_cancellation);
        let connection_slots = Arc::new(Semaphore::new(256));
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _peer)) = accepted else {
                    break;
                };
                let Ok(connection_slot) = Arc::clone(&connection_slots).try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let connection_policy = Arc::clone(&task_policy);
                let connection_development = Arc::clone(&task_development);
                let cancellation = task_cancellation.lock().unwrap().clone();
                tokio::spawn(async move {
                    let _connection_slot = connection_slot;
                    let request_cancellation = cancellation.clone();
                    let service = service_fn(move |request| {
                        handle_request(
                            request,
                            Arc::clone(&connection_policy),
                            Arc::clone(&connection_development),
                            request_cancellation.clone(),
                        )
                    });
                    let connection = hyper::server::conn::http1::Builder::new()
                        .preserve_header_case(true)
                        .title_case_headers(true)
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades();
                    tokio::select! {
                        _ = cancellation.cancelled() => {}
                        result = connection => {
                            if let Err(error) = result {
                                tracing::debug!(%error, "managed browser proxy connection closed");
                            }
                        }
                    }
                });
            }
        });

        Ok(Self {
            address,
            allow_private_network: policy,
            development,
            connection_cancellation,
            shutdown: Some(shutdown_tx),
            task,
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub(super) fn allow_development_url(&self, url: &Url) -> Result<(), String> {
        let endpoint = development_url_endpoint(url).ok_or("仅可授权本机环回端口")?;
        let mut endpoints = self.development.lock().unwrap();
        if endpoints.len() >= 16 && !endpoints.contains(&endpoint) {
            return Err("已达到本地端口授权上限，请先撤销不再使用的授权".into());
        }
        endpoints.insert(endpoint);
        Ok(())
    }

    pub(super) fn permits_development_url(&self, url: &Url) -> bool {
        development_url_endpoint(url)
            .is_some_and(|endpoint| self.development.lock().unwrap().contains(&endpoint))
    }

    pub(super) fn development_endpoints(&self) -> Vec<String> {
        let mut endpoints: Vec<_> = self
            .development
            .lock()
            .unwrap()
            .iter()
            .map(|(host, port)| {
                if host.contains(':') {
                    format!("[{host}]:{port}")
                } else {
                    format!("{host}:{port}")
                }
            })
            .collect();
        endpoints.sort();
        endpoints
    }

    fn clear_development(&self) -> bool {
        let mut endpoints = self.development.lock().unwrap();
        let had_endpoints = !endpoints.is_empty();
        endpoints.clear();
        had_endpoints
    }

    pub(super) fn revoke_development(&self) {
        if self.clear_development() {
            self.cancel_connections();
        }
    }

    fn cancel_connections(&self) {
        let mut cancellation = self.connection_cancellation.lock().unwrap();
        cancellation.cancel();
        *cancellation = CancellationToken::new();
    }

    pub(super) fn allows_private_network(&self) -> bool {
        self.allow_private_network.load(Ordering::Acquire)
    }

    pub fn set_allow_private_network(&self, allowed: bool) {
        let previous = self.allow_private_network.swap(allowed, Ordering::AcqRel);
        let revoked = self.clear_development();
        if previous != allowed || revoked {
            self.cancel_connections();
        }
    }
}

impl Drop for BrowserNetworkProxy {
    fn drop(&mut self) {
        self.connection_cancellation.lock().unwrap().cancel();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

async fn handle_request(
    mut request: Request<hyper::body::Incoming>,
    allow_private_network: Arc<AtomicBool>,
    development: DevelopmentEndpoints,
    cancellation: CancellationToken,
) -> Result<Response<Body>, Infallible> {
    let response = if request.method() == Method::CONNECT {
        handle_connect(
            &mut request,
            allow_private_network.load(Ordering::Acquire),
            &development,
            cancellation,
        )
        .await
    } else {
        forward_http(
            request,
            allow_private_network.load(Ordering::Acquire),
            &development,
            cancellation,
        )
        .await
    };
    Ok(response.unwrap_or_else(error_response))
}

async fn handle_connect(
    request: &mut Request<hyper::body::Incoming>,
    allow_private_network: bool,
    development: &DevelopmentEndpoints,
    cancellation: CancellationToken,
) -> Result<Response<Body>, String> {
    let authority = request
        .uri()
        .authority()
        .ok_or_else(|| "CONNECT 请求缺少目标地址".to_string())?;
    let host = authority.host().to_string();
    let port = authority.port_u16().unwrap_or(443);
    let destinations =
        resolve_browser_destinations(&host, port, allow_private_network, development).await?;
    let upstream =
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(destinations.as_slice()))
            .await
            .map_err(|_| format!("连接 {host}:{port} 超时"))?
            .map_err(|error| format!("连接 {host}:{port} 失败：{error}"))?;
    let upgrade = hyper::upgrade::on(request);
    tokio::spawn(async move {
        match upgrade.await {
            Ok(upgraded) => {
                let mut browser = TokioIo::new(upgraded);
                let mut upstream = upstream;
                tokio::select! {
                    _ = cancellation.cancelled() => {}
                    _ = copy_bidirectional(&mut browser, &mut upstream) => {}
                }
            }
            Err(error) => tracing::debug!(%error, "browser proxy CONNECT upgrade failed"),
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::empty())
        .map_err(|error| format!("创建 CONNECT 响应失败：{error}"))
}

async fn forward_http(
    request: Request<hyper::body::Incoming>,
    allow_private_network: bool,
    development: &DevelopmentEndpoints,
    cancellation: CancellationToken,
) -> Result<Response<Body>, String> {
    let url = Url::parse(&request.uri().to_string())
        .map_err(|error| format!("无效的代理请求地址：{error}"))?;
    if url.scheme() != "http" {
        return Err("代理仅允许 HTTP 或 HTTPS 请求".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("不允许在网址中携带身份凭据".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "代理请求缺少主机名".to_string())?;
    let port = url.port_or_known_default().unwrap_or(80);
    let destinations =
        resolve_browser_destinations(host, port, allow_private_network, development).await?;

    if header_has_token(request.headers(), CONNECTION, "upgrade")
        && request.headers().contains_key(UPGRADE)
    {
        return forward_http_upgrade(request, &url, &destinations, cancellation).await;
    }

    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .resolve_to_addrs(host, &destinations)
        .build()
        .map_err(|error| format!("创建代理请求失败：{error}"))?;

    let method = request.method().clone();
    let mut builder = client.request(method, url.as_str());
    for (name, value) in request.headers() {
        if !is_hop_by_hop(name) && name != HOST {
            builder = builder.header(name, value);
        }
    }
    let body = reqwest::Body::wrap_stream(Body::new(request.into_body()).into_data_stream());
    let upstream = builder
        .body(body)
        .send()
        .await
        .map_err(|error| format!("代理请求失败：{error}"))?;

    let status = upstream.status();
    let mut response = Response::builder().status(status);
    for (name, value) in upstream.headers() {
        if !is_hop_by_hop(name) {
            response = response.header(name, value);
        }
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|error| format!("创建代理响应失败：{error}"))
}

async fn forward_http_upgrade(
    mut request: Request<hyper::body::Incoming>,
    url: &Url,
    destinations: &[SocketAddr],
    cancellation: CancellationToken,
) -> Result<Response<Body>, String> {
    let upstream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(destinations))
        .await
        .map_err(|_| format!("连接 {} 超时", url.host_str().unwrap_or_default()))?
        .map_err(|error| format!("连接 {} 失败：{error}", url.host_str().unwrap_or_default()))?;
    let (mut sender, connection) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        hyper::client::conn::http1::handshake::<_, hyper::body::Incoming>(TokioIo::new(upstream)),
    )
    .await
    .map_err(|_| "建立 WebSocket 上游连接超时".to_string())?
    .map_err(|error| format!("建立 WebSocket 上游连接失败：{error}"))?;
    let connection_cancellation = cancellation.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = connection_cancellation.cancelled() => {}
            result = connection.with_upgrades() => {
                if let Err(error) = result {
                    tracing::debug!(%error, "browser proxy upgrade connection closed");
                }
            }
        }
    });

    let browser_upgrade = hyper::upgrade::on(&mut request);
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    *request.uri_mut() = path
        .parse()
        .map_err(|error| format!("构建 WebSocket 上游路径失败：{error}"))?;
    request.headers_mut().remove(PROXY_AUTHORIZATION);
    let mut upstream_response = tokio::time::timeout(REQUEST_TIMEOUT, sender.send_request(request))
        .await
        .map_err(|_| "WebSocket 上游响应超时".to_string())?
        .map_err(|error| format!("WebSocket 上游请求失败：{error}"))?;

    if upstream_response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut upstream_response);
        tokio::spawn(async move {
            let tunnel = async {
                let (browser, upstream) = tokio::try_join!(browser_upgrade, upstream_upgrade)
                    .map_err(|error| format!("WebSocket 升级失败：{error}"))?;
                let mut browser = TokioIo::new(browser);
                let mut upstream = TokioIo::new(upstream);
                copy_bidirectional(&mut browser, &mut upstream)
                    .await
                    .map_err(|error| format!("WebSocket 转发失败：{error}"))?;
                Ok::<(), String>(())
            };
            tokio::select! {
                _ = cancellation.cancelled() => {}
                result = tunnel => {
                    if let Err(error) = result {
                        tracing::debug!(%error, "browser proxy WebSocket tunnel closed");
                    }
                }
            }
        });
    }
    Ok(upstream_response.map(Body::new))
}

fn header_has_token(
    headers: &hyper::HeaderMap,
    name: hyper::header::HeaderName,
    expected: &str,
) -> bool {
    headers.get_all(name).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case(expected))
        })
    })
}

fn is_hop_by_hop(name: &hyper::header::HeaderName) -> bool {
    matches!(
        *name,
        CONNECTION
            | PROXY_AUTHENTICATE
            | PROXY_AUTHORIZATION
            | TE
            | TRAILER
            | TRANSFER_ENCODING
            | UPGRADE
    )
}

fn error_response(message: String) -> Response<Body> {
    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header("content-type", "text/plain; charset=utf-8")
        .header(CONNECTION, "close")
        .body(Body::from(message))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) async fn resolve_allowed_destinations(
    host: &str,
    port: u16,
    allow_private_network: bool,
    managed_browser: bool,
) -> Result<Vec<SocketAddr>, String> {
    let host = host.trim_matches(['[', ']']);
    let addresses = tokio::time::timeout(
        CONNECT_TIMEOUT,
        lookup_destination(host, port, managed_browser && !allow_private_network),
    )
    .await
    .map_err(|_| {
        tracing::warn!(host, "browser DNS lookup timed out");
        format!("解析目标主机 {host} 超时")
    })?
    .map_err(|error| format!("无法解析目标主机 {host}：{error}"))?;
    if addresses.is_empty() {
        return Err(format!("目标主机 {host} 没有可用地址"));
    }
    if !allow_private_network {
        if let Some(blocked) = addresses
            .iter()
            .find(|address| is_private_or_local(address.ip()))
        {
            return Err(format!(
                "已阻止访问本机或私有网络地址 {} ({})",
                host,
                blocked.ip()
            ));
        }
    }
    Ok(addresses)
}

async fn lookup_destination(
    host: &str,
    port: u16,
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(unused_variables))]
    allow_interface_dns: bool,
) -> std::io::Result<Vec<SocketAddr>> {
    let system = async { Ok(lookup_host((host, port)).await?.collect()) };
    #[cfg(any(target_os = "macos", windows))]
    if allow_interface_dns && interface_dns::is_public_dns_name(host) {
        return interface_dns::with_fallback(system, interface_dns::lookup(host, port)).await;
    }
    system.await
}

pub(crate) fn is_private_or_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1])
                || ip.octets()[0] == 198 && (18..=19).contains(&ip.octets()[1])
                || ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0
                || ip.octets()[0] == 192 && ip.octets()[1] == 88 && ip.octets()[2] == 99
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.segments()[0] & 0xffc0 == 0xfec0
                || is_ipv6_documentation(ip)
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_private_or_local(IpAddr::V4(mapped)))
        }
    }
}

fn is_ipv6_documentation(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn blocks_non_public_address_ranges() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.1.1",
            "192.0.0.1",
            "192.88.99.1",
            "192.168.1.1",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "fec0::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_private_or_local(address.parse().unwrap()), "{address}");
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!is_private_or_local(address.parse().unwrap()), "{address}");
        }
    }

    #[tokio::test]
    async fn resolver_rejects_loopback_unless_explicitly_allowed() {
        assert!(resolve_allowed_destinations("127.0.0.1", 80, false, true)
            .await
            .is_err());
        assert!(resolve_allowed_destinations("127.0.0.1", 80, true, true)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn proxy_enforces_and_reloads_private_network_policy() {
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut request = vec![0_u8; 4096];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await
                .unwrap();
        });
        let proxy = BrowserNetworkProxy::start(false).await.unwrap();
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.address())).unwrap())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let url = format!("http://{upstream_address}/health");

        let denied = client.get(&url).send().await.unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        proxy.set_allow_private_network(true);
        let refreshed_client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.address())).unwrap())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let allowed = refreshed_client.get(&url).send().await.unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(allowed.text().await.unwrap(), "ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn approved_localhost_uses_ipv4_fallback_and_revokes_only_the_grant() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route("/health", axum::routing::get(|| async { "ok" })),
            )
            .await
        });
        let proxy = BrowserNetworkProxy::start(false).await.unwrap();
        let url = Url::parse(&format!("http://localhost:{port}/health")).unwrap();
        let client = || {
            reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{}", proxy.address())).unwrap())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap()
        };
        assert_eq!(
            client().get(url.clone()).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        proxy.allow_development_url(&url).unwrap();
        assert_eq!(
            proxy.development_endpoints(),
            vec![format!("localhost:{port}")]
        );
        assert_eq!(
            client()
                .get(url.clone())
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        assert!(!proxy
            .permits_development_url(&Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()));
        proxy.revoke_development();
        assert!(proxy.development_endpoints().is_empty());
        assert_eq!(
            client().get(url).send().await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        server.abort();
    }

    #[test]
    fn development_endpoint_rejects_private_lan_and_keeps_default_ports() {
        assert!(
            development_url_endpoint(&Url::parse("http://192.168.1.2:5173/").unwrap()).is_none()
        );
        assert_eq!(
            development_url_endpoint(&Url::parse("http://localhost:80/").unwrap()),
            Some(("localhost".into(), 80))
        );
    }

    #[tokio::test]
    async fn proxy_tunnels_http_upgrade_without_bypassing_policy() {
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream_address = upstream.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 512];
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            assert!(String::from_utf8_lossy(&request).starts_with("GET /socket HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nconnection: Upgrade\r\nupgrade: echo\r\n\r\n",
                )
                .await
                .unwrap();
            let mut message = [0_u8; 4];
            stream.read_exact(&mut message).await.unwrap();
            assert_eq!(&message, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let proxy = BrowserNetworkProxy::start(false).await.unwrap();
        let mut denied = TcpStream::connect(proxy.address()).await.unwrap();
        denied
            .write_all(
                format!(
                    "GET http://{upstream_address}/socket HTTP/1.1\r\nHost: {upstream_address}\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut denied_response = [0_u8; 512];
        let denied_read = denied.read(&mut denied_response).await.unwrap();
        assert!(String::from_utf8_lossy(&denied_response[..denied_read]).contains("403 Forbidden"));

        proxy.set_allow_private_network(true);
        let mut client = TcpStream::connect(proxy.address()).await.unwrap();
        client
            .write_all(
                format!(
                    "GET http://{upstream_address}/socket HTTP/1.1\r\nHost: {upstream_address}\r\nConnection: Upgrade\r\nUpgrade: echo\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        loop {
            let mut chunk = [0_u8; 512];
            let read = client.read(&mut chunk).await.unwrap();
            assert!(read > 0);
            response.extend_from_slice(&chunk[..read]);
            if response.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 101"));
        client.write_all(b"ping").await.unwrap();
        let mut reply = [0_u8; 4];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"pong");
        server.await.unwrap();
    }
}
