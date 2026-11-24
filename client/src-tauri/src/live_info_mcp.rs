//! Built-in, keyless weather and web search for ordinary Agent conversations.
//! Only this process can connect to the loopback MCP endpoint. The two tools
//! make read-only requests to fixed public hosts and never execute model input.

use axum::{
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use futures::StreamExt;
use quick_xml::events::Event;
use quick_xml::Reader;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MCP_SERVER_NAME: &str = "echoagent-live-info";
const BODY_LIMIT: usize = 16 * 1024;
const MAX_UPSTREAM_BYTES: usize = 512 * 1024;
const WEATHER_TTL: Duration = Duration::from_secs(5 * 60);
const SEARCH_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_CACHE_ENTRIES: usize = 128;
static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
static PERSISTED: Mutex<bool> = Mutex::new(false);

struct Service {
    port: u16,
    token: String,
    ready: AtomicBool,
}

#[derive(Clone)]
struct ServerState {
    authorization: String,
    expected_host: String,
    http: reqwest::Client,
    http_direct: reqwest::Client,
    cache: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

struct CacheEntry {
    value: Value,
    saved_at: Instant,
}

#[derive(Deserialize)]
struct RpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WeatherArgs {
    location: String,
    #[serde(
        default = "default_days",
        deserialize_with = "deserialize_weather_days"
    )]
    days: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_count")]
    count: usize,
}

fn default_days() -> usize {
    3
}

fn deserialize_weather_days<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<usize, D::Error> {
    let value = Value::deserialize(deserializer)?;
    match (value.as_u64(), value.as_str()) {
        (Some(1), _) | (_, Some("1")) => Ok(1),
        (Some(2), _) | (_, Some("2")) => Ok(2),
        (Some(3), _) | (_, Some("3")) => Ok(3),
        _ => Err(serde::de::Error::custom(
            "days 应为 1 到 3 的整数，也可以传 \"1\"、\"2\" 或 \"3\"",
        )),
    }
}

fn default_count() -> usize {
    5
}

pub fn serve() -> Result<(), String> {
    if SERVICE.get().is_some() {
        return Ok(());
    }
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|error| format!("实时信息服务无法绑定本地端口：{error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let service = Arc::new(Service {
        port,
        token: uuid::Uuid::now_v7().to_string(),
        ready: AtomicBool::new(false),
    });
    let http = http_client_builder()
        .build()
        .map_err(|error| format!("实时信息 HTTP 客户端启动失败：{error}"))?;
    let http_direct = http_client_builder()
        .no_proxy()
        .build()
        .map_err(|error| format!("实时信息直连客户端启动失败：{error}"))?;
    let state = ServerState {
        authorization: format!("Bearer {}", service.token),
        expected_host: format!("127.0.0.1:{port}"),
        http,
        http_direct,
        cache: Arc::new(Mutex::new(HashMap::new())),
    };
    SERVICE
        .set(service.clone())
        .map_err(|_| "实时信息服务重复启动".to_string())?;
    let (startup_tx, startup_rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = startup_tx.send(Err(format!("实时信息服务启动失败：{error}")));
                return;
            }
        };
        let router = Router::new()
            .route("/mcp", post(handle_post))
            .layer(DefaultBodyLimit::max(BODY_LIMIT))
            .with_state(state);
        service.ready.store(true, Ordering::Release);
        let _ = startup_tx.send(Ok(()));
        if let Err(error) = axum::serve(listener, router).await {
            tracing::error!(%error, "live information MCP server stopped");
        }
        service.ready.store(false, Ordering::Release);
        *PERSISTED.lock().unwrap() = false;
    });
    startup_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| format!("等待实时信息服务启动超时：{error}"))?
}

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            const HOSTS: &[&str] = &[
                "wttr.in",
                "wttr.is",
                "www.bing.com",
                "cn.bing.com",
                "html.duckduckgo.com",
                "duckduckgo.com",
            ];
            if attempt.previous().len() < 3
                && attempt.url().scheme() == "https"
                && attempt
                    .url()
                    .host_str()
                    .is_some_and(|host| HOSTS.contains(&host))
            {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .user_agent(concat!("EchoAgent/", env!("CARGO_PKG_VERSION")))
}

pub fn server_url() -> Option<String> {
    SERVICE.get().and_then(|service| {
        service
            .ready
            .load(Ordering::Acquire)
            .then(|| format!("http://127.0.0.1:{}/mcp", service.port))
    })
}

pub fn authorization_header() -> Option<String> {
    SERVICE.get().and_then(|service| {
        service
            .ready
            .load(Ordering::Acquire)
            .then(|| format!("Bearer {}", service.token))
    })
}

fn valid_headers(headers: &HeaderMap, state: &ServerState) -> Result<(), StatusCode> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if auth.len() != state.authorization.len()
        || auth
            .as_bytes()
            .iter()
            .zip(state.authorization.as_bytes())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            != 0
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        != Some(state.expected_host.as_str())
    {
        return Err(StatusCode::MISDIRECTED_REQUEST);
    }
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/json"))
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    Ok(())
}

async fn handle_post(
    State(state): State<ServerState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(status) = valid_headers(&headers, &state) {
        return status.into_response();
    }
    if body.len() > BODY_LIMIT {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let request: RpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => return rpc_error(Value::Null, -32700, format!("invalid JSON: {error}")),
    };
    let Some(id) = request.id else {
        return StatusCode::ACCEPTED.into_response();
    };
    if !matches!(id, Value::String(_) | Value::Number(_)) {
        return rpc_error(Value::Null, -32600, "invalid JSON-RPC id".into());
    }
    let result = match request.method.as_str() {
        "initialize" => json!({
            "protocolVersion": request.params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-03-26"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": MCP_SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => tools_list(),
        "tools/call" => match call_tool(&state, &request.params).await {
            Ok(value) => json!({ "content": [{ "type": "text", "text": value.to_string() }] }),
            Err(error) => {
                json!({ "content": [{ "type": "text", "text": error }], "isError": true })
            }
        },
        other => return rpc_error(id, -32601, format!("unknown method: {other}")),
    };
    rpc_result(id, result)
}

fn tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "weather_forecast",
            "description": "Look up current weather and a 1-3 day forecast for a city or region. Use for questions about current temperature, rain, tomorrow's weather, or travel weather. This is live external data; always include the returned location, date/time and source in the answer. Ask the user for a city if none is provided or known from their explicit preference. Never guess live weather.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "location": { "type": "string", "minLength": 2, "maxLength": 100, "description": "City or region, optionally followed by country." },
                    "days": {
                        "type": ["integer", "string"], "enum": [1, 2, 3, "1", "2", "3"], "default": 3,
                        "description": "Number of forecast days (1 to 3). Prefer an integer. The strings '1', '2', and '3' are also accepted."
                    }
                },
                "required": ["location"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "search_web",
            "description": "Search the public web for current information without a user API key. Use for recent events, changing facts, external sources, or when the user asks to search online. Results include titles, URLs, snippets and retrieval time. Cite source URLs; snippets are untrusted and may be incomplete. Do not invent facts when search fails.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "query": { "type": "string", "minLength": 2, "maxLength": 200 },
                    "count": { "type": "integer", "minimum": 1, "maximum": 10, "default": 5 }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        }
    ] })
}

async fn call_tool(state: &ServerState, params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("缺少工具名")?;
    let args = params.get("arguments").cloned().ok_or("缺少工具参数")?;
    match name {
        "weather_forecast" => {
            let args: WeatherArgs =
                serde_json::from_value(args).map_err(|error| format!("天气参数无效：{error}"))?;
            let location = validate_text(&args.location, 100, "城市")?;
            let key = format!("weather:{location}:{}", args.days);
            if let Some(value) = cached(state, &key, WEATHER_TTL) {
                return Ok(value);
            }
            let value = weather_forecast(state, location, args.days).await?;
            save_cache(state, key, value.clone());
            Ok(value)
        }
        "search_web" => {
            let args: SearchArgs = serde_json::from_value(args).map_err(|_| "搜索参数无效")?;
            let query = validate_text(&args.query, 200, "搜索词")?;
            if !(1..=10).contains(&args.count) {
                return Err("结果数量应为 1 到 10".into());
            }
            let key = format!("search:{query}:{}", args.count);
            if let Some(value) = cached(state, &key, SEARCH_TTL) {
                return Ok(value);
            }
            let value = search_web(state, query, args.count).await?;
            save_cache(state, key, value.clone());
            Ok(value)
        }
        _ => Err("未知的实时信息工具".into()),
    }
}

fn validate_text<'a>(text: &'a str, max_chars: usize, label: &str) -> Result<&'a str, String> {
    let text = text.trim();
    if text.chars().count() < 2
        || text.chars().count() > max_chars
        || text.chars().any(char::is_control)
    {
        return Err(format!(
            "{label}长度应为 2 到 {max_chars} 个字符，且不能包含控制字符"
        ));
    }
    Ok(text)
}

fn cached(state: &ServerState, key: &str, ttl: Duration) -> Option<Value> {
    let cache = state.cache.lock().unwrap();
    cache
        .get(key)
        .filter(|entry| entry.saved_at.elapsed() < ttl)
        .map(|entry| {
            let mut value = entry.value.clone();
            value["cached"] = json!(true);
            value
        })
}

fn save_cache(state: &ServerState, key: String, value: Value) {
    let mut cache = state.cache.lock().unwrap();
    cache.retain(|_, entry| entry.saved_at.elapsed() < SEARCH_TTL);
    if cache.len() >= MAX_CACHE_ENTRIES {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.saved_at)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(
        key,
        CacheEntry {
            value,
            saved_at: Instant::now(),
        },
    );
}

async fn read_bounded(response: reqwest::Response) -> Result<String, String> {
    if !response.status().is_success() {
        return Err(format!("上游服务返回 HTTP {}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_UPSTREAM_BYTES as u64)
    {
        return Err("上游响应过大".into());
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取上游响应失败：{error}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_UPSTREAM_BYTES {
            return Err("上游响应过大".into());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "上游响应不是 UTF-8 文本".into())
}

async fn get_public_url(
    state: &ServerState,
    url: reqwest::Url,
) -> Result<reqwest::Response, String> {
    match state.http.get(url.clone()).send().await {
        Ok(response) => Ok(response),
        Err(proxy_error) => state
            .http_direct
            .get(url)
            .send()
            .await
            .map_err(|direct_error| {
                format!("网络连接失败（代理：{proxy_error}；直连：{direct_error}）")
            }),
    }
}

async fn weather_forecast(
    state: &ServerState,
    location: &str,
    days: usize,
) -> Result<Value, String> {
    let mut errors = Vec::new();
    for host in ["wttr.in", "wttr.is"] {
        let mut url =
            reqwest::Url::parse(&format!("https://{host}/")).map_err(|error| error.to_string())?;
        url.path_segments_mut()
            .map_err(|_| "天气地址无效")?
            .push(location);
        url.query_pairs_mut().append_pair("format", "j1");
        match get_public_url(state, url.clone()).await {
            Ok(response) => match read_bounded(response)
                .await
                .and_then(|body| {
                    serde_json::from_str::<Value>(&body)
                        .map_err(|_| "天气服务响应格式无效".to_string())
                })
                .and_then(|body| normalize_weather(&body, location, days, url.as_str()))
            {
                Ok(value) => return Ok(value),
                Err(error) => errors.push(format!("{host}: {error}")),
            },
            Err(error) => errors.push(format!("{host}: {error}")),
        }
    }
    Err(format!(
        "暂时无法获取 {location} 的天气。{}",
        errors.join("；")
    ))
}

fn weather_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn nested_weather_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .and_then(|array| array.first())
        .and_then(|item| weather_string(item, "value"))
}

fn normalize_weather(
    raw: &Value,
    requested_location: &str,
    days: usize,
    source_url: &str,
) -> Result<Value, String> {
    let current = raw
        .get("current_condition")
        .and_then(Value::as_array)
        .and_then(|array| array.first())
        .ok_or("天气服务未返回当前状况")?;
    let place = raw
        .get("nearest_area")
        .and_then(Value::as_array)
        .and_then(|array| array.first())
        .ok_or("天气服务未返回城市")?;
    let area =
        nested_weather_string(place, "areaName").unwrap_or_else(|| requested_location.to_string());
    let country = nested_weather_string(place, "country");
    let forecasts = raw
        .get("weather")
        .and_then(Value::as_array)
        .ok_or("天气服务未返回预报")?;
    if forecasts.is_empty() {
        return Err("天气服务未返回预报".into());
    }
    let daily = forecasts
        .iter()
        .take(days)
        .map(|day| {
            let hourly =
                day.get("hourly")
                    .and_then(Value::as_array)
                    .map(|hours| {
                        hours.iter().map(|hour| json!({
                "time": weather_string(hour, "time"),
                "rain_probability_percent": weather_string(hour, "chanceofrain"),
                "precipitation_mm": weather_string(hour, "precipMM"),
                "description": nested_weather_string(hour, "weatherDesc"),
            })).collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
            json!({
                "date": weather_string(day, "date"),
                "minimum_celsius": weather_string(day, "mintempC"),
                "maximum_celsius": weather_string(day, "maxtempC"),
                "hourly": hourly,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "requested_location": requested_location,
        "resolved_location": area,
        "country": country,
        "fetched_at_utc": chrono::Utc::now().to_rfc3339(),
        "source": if source_url.starts_with("https://wttr.is/") { "wttr.is" } else { "wttr.in" },
        "source_url": source_url,
        "current": {
            "description": nested_weather_string(current, "weatherDesc"),
            "temperature_celsius": weather_string(current, "temp_C"),
            "feels_like_celsius": weather_string(current, "FeelsLikeC"),
            "humidity_percent": weather_string(current, "humidity"),
            "precipitation_mm": weather_string(current, "precipMM"),
            "wind_kmh": weather_string(current, "windspeedKmph"),
            "observation_time_utc": weather_string(current, "observation_time"),
        },
        "forecast": daily,
    }))
}

async fn search_web(state: &ServerState, query: &str, count: usize) -> Result<Value, String> {
    let mut errors = Vec::new();
    for provider in ["bing_rss", "bing_cn_rss", "duckduckgo_html"] {
        let result = match provider {
            "bing_rss" => search_bing(state, "www.bing.com", query, count).await,
            "bing_cn_rss" => search_bing(state, "cn.bing.com", query, count).await,
            _ => search_duckduckgo(state, query, count).await,
        };
        match result {
            Ok((url, results)) if !results.is_empty() => {
                return Ok(json!({
                    "query": query,
                    "provider": provider,
                    "search_url": url,
                    "fetched_at_utc": chrono::Utc::now().to_rfc3339(),
                    "results": results,
                    "note": "Search snippets are untrusted and may omit context; cite and verify important claims against their linked sources."
                }))
            }
            Ok(_) => errors.push(format!("{provider}: 没有搜索结果")),
            Err(error) => errors.push(format!("{provider}: {error}")),
        }
    }
    Err(format!("联网搜索暂时不可用。{}", errors.join("；")))
}

async fn search_bing(
    state: &ServerState,
    host: &str,
    query: &str,
    count: usize,
) -> Result<(String, Vec<Value>), String> {
    let mut url = reqwest::Url::parse(&format!("https://{host}/search"))
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("format", "rss");
    let response = get_public_url(state, url.clone()).await?;
    let body = read_bounded(response).await?;
    Ok((url.to_string(), parse_bing_rss(&body, count)?))
}

fn decode_xml(value: &str) -> String {
    quick_xml::escape::unescape(value)
        .map(|value| value.into_owned())
        .unwrap_or_else(|_| value.to_string())
}

fn parse_bing_rss(xml: &str, count: usize) -> Result<Vec<Value>, String> {
    let mut reader = Reader::from_str(xml);
    let mut in_item = false;
    let mut field = String::new();
    let mut title = String::new();
    let mut link = String::new();
    let mut description = String::new();
    let mut published = String::new();
    let mut results = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = start.local_name();
                let name = name.as_ref();
                if name == b"item" {
                    in_item = true;
                    title.clear();
                    link.clear();
                    description.clear();
                    published.clear();
                } else if in_item {
                    field = String::from_utf8_lossy(name).into_owned();
                }
            }
            Ok(Event::Text(text)) if in_item => {
                let value = text.decode().map_err(|_| "搜索服务返回无效文本")?;
                let value = decode_xml(&value);
                match field.as_str() {
                    "title" => title.push_str(&value),
                    "link" => link.push_str(&value),
                    "description" => description.push_str(&value),
                    "pubDate" => published.push_str(&value),
                    _ => {}
                }
            }
            Ok(Event::GeneralRef(reference)) if in_item => {
                let value = reference.decode().map_err(|_| "搜索服务返回无效文本")?;
                let value = decode_xml(&format!("&{value};"));
                match field.as_str() {
                    "title" => title.push_str(&value),
                    "link" => link.push_str(&value),
                    "description" => description.push_str(&value),
                    "pubDate" => published.push_str(&value),
                    _ => {}
                }
            }
            Ok(Event::End(end)) => {
                let name = end.local_name();
                if name.as_ref() == b"item" {
                    if valid_public_result_url(&link) && !title.is_empty() {
                        results.push(json!({ "title": title.trim(), "url": link.trim(), "snippet": description.trim(), "published_at": published.trim() }));
                        if results.len() >= count {
                            break;
                        }
                    }
                    in_item = false;
                }
                field.clear();
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err("搜索服务返回无效 RSS".into()),
            _ => {}
        }
    }
    Ok(results)
}

async fn search_duckduckgo(
    state: &ServerState,
    query: &str,
    count: usize,
) -> Result<(String, Vec<Value>), String> {
    let mut url = reqwest::Url::parse("https://html.duckduckgo.com/html/")
        .map_err(|error| error.to_string())?;
    url.query_pairs_mut().append_pair("q", query);
    let response = get_public_url(state, url.clone()).await?;
    let body = read_bounded(response).await?;
    if body.contains("challenge-form") || body.contains("g-recaptcha") {
        return Err("搜索服务要求人工验证".into());
    }
    Ok((url.to_string(), parse_duckduckgo_html(&body, count)?))
}

fn valid_public_result_url(value: &str) -> bool {
    reqwest::Url::parse(value.trim())
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn strip_html(value: &str) -> String {
    let tags = Regex::new(r"(?s)<[^>]*>").expect("valid regex");
    let plain = tags.replace_all(value, " ");
    decode_xml(&plain)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_duckduckgo_html(html: &str, count: usize) -> Result<Vec<Value>, String> {
    let anchor = Regex::new(r#"(?is)<a\b([^>]*)>(.*?)</a>"#).map_err(|error| error.to_string())?;
    let href =
        Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']+)["']"#).map_err(|error| error.to_string())?;
    let mut results = Vec::new();
    for captures in anchor.captures_iter(html) {
        let attributes = captures.get(1).map(|item| item.as_str()).unwrap_or("");
        if !attributes.contains("result__a") {
            continue;
        }
        let raw_url = href
            .captures(attributes)
            .and_then(|item| item.get(1))
            .map(|item| decode_xml(item.as_str()))
            .unwrap_or_default();
        let url = if raw_url.starts_with("//") {
            format!("https:{raw_url}")
        } else {
            raw_url
        };
        let url = reqwest::Url::parse(&url)
            .ok()
            .and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == "uddg")
                    .map(|(_, value)| value.into_owned())
                    .or_else(|| Some(url.to_string()))
            })
            .unwrap_or_default();
        if !valid_public_result_url(&url) {
            continue;
        }
        let title = strip_html(captures.get(2).map(|item| item.as_str()).unwrap_or(""));
        if title.is_empty() {
            continue;
        }
        results.push(json!({ "title": title, "url": url, "snippet": "" }));
        if results.len() >= count {
            break;
        }
    }
    Ok(results)
}

fn rpc_result(id: Value, result: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        json!({"jsonrpc":"2.0","id":id,"result":result}).to_string(),
    )
        .into_response()
}

fn rpc_error(id: Value, code: i64, message: String) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string(),
    )
        .into_response()
}

pub fn persist_registration(tx: &echo_agent_acp::AcpAgentTx, session_id: &str) {
    {
        let mut persisted = PERSISTED.lock().unwrap();
        if *persisted {
            return;
        }
        *persisted = true;
    }
    let (Some(url), Some(authorization)) = (server_url(), authorization_header()) else {
        *PERSISTED.lock().unwrap() = false;
        return;
    };
    let tx = tx.clone();
    let session_id = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let payload = json!({
            "session_id": session_id,
            "server_name": MCP_SERVER_NAME,
            "url": url,
            "headers": { "Authorization": authorization },
            "enabled": true,
        });
        if let Err(error) = crate::ext::call_ext_value(
            &tx,
            "echo.agent/mcp/upsert",
            crate::ext::raw_params(&payload),
        )
        .await
        {
            tracing::warn!(?error, "live information MCP registration failed");
            *PERSISTED.lock().unwrap() = false;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_normalization_keeps_rain_forecast_and_source() {
        let raw = json!({
            "nearest_area": [{"areaName":[{"value":"Beijing"}],"country":[{"value":"China"}]}],
            "current_condition": [{"temp_C":"23","FeelsLikeC":"22","weatherDesc":[{"value":"Clear"}]}],
            "weather": [{"date":"2026-09-28","mintempC":"18","maxtempC":"25","hourly":[{"time":"1200","chanceofrain":"75","precipMM":"0.4"}]}]
        });
        let result =
            normalize_weather(&raw, "北京", 1, "https://wttr.in/Beijing?format=j1").unwrap();
        assert_eq!(result["resolved_location"], "Beijing");
        assert_eq!(
            result["forecast"][0]["hourly"][0]["rain_probability_percent"],
            "75"
        );
        assert_eq!(result["source"], "wttr.in");
    }

    #[test]
    fn bing_rss_parses_results_and_ignores_bad_links() {
        let xml = r#"<rss><channel><item><title>A &amp; B</title><link>https://example.com/a</link><description>Current &lt;report&gt;</description><pubDate>today</pubDate></item><item><title>Bad</title><link>file:///etc/passwd</link></item></channel></rss>"#;
        let results = parse_bing_rss(xml, 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["title"], "A & B");
        assert_eq!(results[0]["snippet"], "Current <report>");
    }

    #[test]
    fn rejects_invalid_tool_inputs() {
        assert!(validate_text("\n", 100, "城市").is_err());
        assert!(validate_text("a", 100, "城市").is_err());
        assert!(validate_text("北京", 100, "城市").is_ok());
    }

    #[tokio::test]
    async fn weather_accepts_string_days_and_reports_invalid_days() {
        let state = ServerState {
            authorization: String::new(),
            expected_host: String::new(),
            http: reqwest::Client::new(),
            http_direct: reqwest::Client::new(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        };
        save_cache(
            &state,
            "weather:北京:3".into(),
            json!({"requested_location": "北京"}),
        );
        for arguments in [
            json!({"location": "北京", "days": "3"}),
            json!({"location": "北京", "days": 3}),
            json!({"location": "北京"}),
        ] {
            let params = json!({"name": "weather_forecast", "arguments": arguments});
            let result = call_tool(&state, &params).await.unwrap();
            assert_eq!(
                result,
                json!({"requested_location": "北京", "cached": true})
            );
        }

        for days in [
            json!("4"),
            json!("03"),
            json!(4),
            json!(1.5),
            json!(3.0),
            Value::Null,
        ] {
            let params = json!({
                "name": "weather_forecast", "arguments": {"location": "北京", "days": days},
            });
            let error = call_tool(&state, &params).await.unwrap_err();
            assert!(error.contains("days 应为 1 到 3"), "{error}");
        }
    }

    #[tokio::test]
    async fn mcp_lists_both_tools_only_with_local_authorization() {
        let state = ServerState {
            authorization: "Bearer test-token".into(),
            expected_host: "127.0.0.1:10000".into(),
            http: reqwest::Client::new(),
            http_direct: reqwest::Client::new(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:10000".parse().unwrap());
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        headers.insert(header::AUTHORIZATION, "Bearer test-token".parse().unwrap());
        let request =
            axum::body::Bytes::from_static(br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let response = handle_post(State(state.clone()), headers.clone(), request.clone()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let rpc: Value = serde_json::from_slice(&body).unwrap();
        let names = rpc["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["weather_forecast", "search_web"]);

        headers.remove(header::AUTHORIZATION);
        let denied = handle_post(State(state), headers, request).await;
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }
}
