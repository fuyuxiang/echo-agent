use super::network_proxy::{resolve_allowed_destination, BrowserNetworkProxy};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

const START_TIMEOUT: Duration = Duration::from_secs(12);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CDP_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const DEEP_QUERY_JS: &str = include_str!("deep-query.js");
const EDITABLE_TEXT_JS: &str = include_str!("editable-text.js");
const INTERACTION_TARGET_JS: &str = include_str!("interaction-target.js");
type CdpConnection = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub(super) struct BrowserInteraction {
    target_id: String,
    context_id: u64,
    input: Value,
    expected: Value,
    offset: (f64, f64),
    frame_id: Option<String>,
}

impl BrowserInteraction {
    pub(super) fn target(&self) -> Value {
        self.expected["target"]["preview"].clone()
    }

    pub(super) fn destination(&self) -> Value {
        self.expected["destination"]["preview"].clone()
    }

    pub(super) fn form_submit_url(&self) -> Option<String> {
        let url = Url::parse(self.expected["submission"]["action"].as_str()?).ok()?;
        Some(url[..url::Position::AfterPath].chars().take(300).collect())
    }

    pub(super) fn url(&self) -> &str {
        self.expected["topUrl"]
            .as_str()
            .or(self.expected["url"].as_str())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCapability {
    pub available: bool,
    pub browser_name: Option<String>,
    pub executable: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTarget {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "type", default)]
    pub target_type: String,
    #[serde(rename = "webSocketDebuggerUrl", default)]
    pub websocket_debugger_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    pub running: bool,
    pub browser_name: Option<String>,
    pub target_id: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub profile_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserDownload {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub modified_at_ms: u64,
    pub complete: bool,
}

/// One user-visible, automation-owned browser process. Calls are serialized by
/// the session manager, so a CDP command can never race another action in the
/// same task.
pub struct BrowserController {
    child: Option<Child>,
    extension: Option<std::sync::Arc<super::browser_bridge::Connection>>,
    page_connection: Option<(String, CdpConnection)>,
    active_frame: Option<String>,
    snapshot_pages: std::collections::HashMap<String, (std::time::Instant, String, Value)>,
    reference_documents: std::collections::HashMap<String, (std::time::Instant, String, String)>,
    _protection: Option<echo_agent_tools::util::process_safety::ProcessLease>,
    port: u16,
    browser_name: String,
    profile_dir: PathBuf,
    target_id: Option<String>,
    allow_private_network: bool,
    network_proxy: Option<BrowserNetworkProxy>,
    next_cdp_id: u64,
    // Chromium releases download overrides when their DevTools session ends.
    // Keep a browser-level connection alive across page commands and tabs.
    download_session: Option<CdpConnection>,
}

impl std::fmt::Debug for BrowserController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserController")
            .field("port", &self.port)
            .field("browser_name", &self.browser_name)
            .field("profile_dir", &self.profile_dir)
            .field("target_id", &self.target_id)
            .finish_non_exhaustive()
    }
}

impl Drop for BrowserController {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
    }
}

impl BrowserController {
    pub async fn launch(session_id: &str, allow_private_network: bool) -> Result<Self, String> {
        if let Some(extension) = super::browser_bridge::connected(session_id) {
            extension.resume().await?;
            let mut browser = Self {
                child: None,
                extension: Some(extension),
                page_connection: None,
                active_frame: None,
                snapshot_pages: Default::default(),
                reference_documents: Default::default(),
                _protection: None,
                port: 0,
                browser_name: "已授权的浏览器扩展".into(),
                profile_dir: profile_dir(session_id)?,
                target_id: None,
                allow_private_network,
                network_proxy: None,
                next_cdp_id: 1,
                download_session: None,
            };
            let target = browser.ensure_page().await?;
            browser.target_id = Some(target.id);
            return Ok(browser);
        }
        let (browser_name, executable) = discover_browser().ok_or_else(|| {
            "未找到可控制的浏览器。请安装 Google Chrome、Microsoft Edge 或 Chromium 后重试。"
                .to_string()
        })?;
        let profile_dir = profile_dir(session_id)?;
        std::fs::create_dir_all(&profile_dir)
            .map_err(|error| format!("创建受控浏览器配置目录失败：{error}"))?;
        crate::paths::harden_private_dir(&profile_dir)?;
        let active_port_file = profile_dir.join("DevToolsActivePort");
        if active_port_file.exists() {
            let _ = std::fs::remove_file(&active_port_file);
        }

        let network_proxy = BrowserNetworkProxy::start(allow_private_network).await?;

        let mut command = Command::new(&executable);
        #[cfg(test)]
        if std::env::var_os("ECHO_BROWSER_TEST_HEADLESS").is_some() {
            command.arg("--headless=new");
        }
        command
            .arg("--remote-debugging-address=127.0.0.1")
            .arg("--remote-debugging-port=0")
            .arg(format!("--user-data-dir={}", profile_dir.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-sync")
            .arg("--disable-background-mode")
            .arg("--disable-background-networking")
            .arg("--disable-component-update")
            .arg("--disable-default-apps")
            .arg("--disable-extensions")
            .arg("--disable-quic")
            .arg("--no-service-autorun")
            .arg("--force-webrtc-ip-handling-policy=disable_non_proxied_udp")
            .arg(format!("--proxy-server=http://{}", network_proxy.address()))
            // Chromium implicitly bypasses proxies for loopback destinations.
            // Remove that bypass so localhost and private-network requests are
            // subject to the same user-controlled policy as every subresource.
            .arg("--proxy-bypass-list=<-loopback>")
            .arg("--window-size=1280,900")
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|error| format!("启动 {browser_name} 失败：{error}"))?;

        let port = wait_for_debug_port(&active_port_file, START_TIMEOUT).await?;
        let protection = echo_agent_tools::util::process_safety::protect_tree(
            child.id().ok_or("浏览器在身份核验前已退出")?,
        )?;
        let mut controller = Self {
            child: Some(child),
            extension: None,
            page_connection: None,
            active_frame: None,
            snapshot_pages: Default::default(),
            reference_documents: Default::default(),
            _protection: protection,
            port,
            browser_name,
            profile_dir,
            target_id: None,
            allow_private_network,
            network_proxy: Some(network_proxy),
            next_cdp_id: 1,
            download_session: None,
        };
        let target = controller.ensure_page().await?;
        controller.target_id = Some(target.id);
        let download_dir = controller.download_dir();
        std::fs::create_dir_all(&download_dir)
            .map_err(|error| format!("创建浏览器下载目录失败：{error}"))?;
        crate::paths::harden_private_dir(&download_dir)?;
        let download_path = download_dir.to_string_lossy().into_owned();
        let configure_downloads = async {
            let endpoint = format!("http://127.0.0.1:{}/json/version", controller.port);
            let version: Value = control_client()?
                .get(endpoint)
                .send()
                .await
                .map_err(|error| format!("读取浏览器下载控制地址失败：{error}"))?
                .error_for_status()
                .map_err(|error| format!("读取浏览器下载控制地址失败：{error}"))?
                .json()
                .await
                .map_err(|error| format!("解析浏览器下载控制地址失败：{error}"))?;
            let address = version
                .get("webSocketDebuggerUrl")
                .and_then(Value::as_str)
                .ok_or_else(|| "浏览器未提供下载控制连接".to_string())?;
            let (mut connection, _) = tokio_tungstenite::connect_async(address)
                .await
                .map_err(|error| format!("连接浏览器下载控制失败：{error}"))?;
            let id = controller.next_cdp_id;
            controller.next_cdp_id = controller.next_cdp_id.saturating_add(1);
            cdp_request(
                &mut connection,
                id,
                "Browser.setDownloadBehavior",
                json!({
                    "behavior": "allow", "downloadPath": download_path, "eventsEnabled": false
                }),
            )
            .await?;
            controller.download_session = Some(connection);
            Ok::<(), String>(())
        };
        tokio::time::timeout(COMMAND_TIMEOUT, configure_downloads)
            .await
            .map_err(|_| "配置浏览器任务下载目录超时".to_string())??;
        Ok(controller)
    }

    pub fn set_allow_private_network(&mut self, allowed: bool) {
        self.allow_private_network = allowed;
        if let Some(proxy) = &self.network_proxy {
            proxy.set_allow_private_network(allowed);
        }
    }

    /// Re-check the live page rather than trusting only the URL originally
    /// requested by the model. Links, form submissions and page scripts can
    /// all redirect independently after navigation.
    pub async fn enforce_current_url_policy(&mut self) -> Result<String, String> {
        let current = self.current_url().await?;
        if let Err(error) = validate_navigation_url(&current, self.allow_private_network).await {
            let _ = self
                .call_page("Page.navigate", json!({ "url": "about:blank" }))
                .await;
            return Err(format!("页面进入了未获授权的地址，已返回空白页：{error}"));
        }
        Ok(current)
    }

    pub async fn status(&mut self) -> Result<BrowserStatus, String> {
        if self.extension.as_ref().is_some_and(|c| !c.live()) {
            return Err("browser_extension_disconnected".into());
        }
        if self
            .child
            .as_mut()
            .map(|child| child.try_wait())
            .transpose()
            .map_err(|error| format!("检查浏览器进程失败：{error}"))?
            .flatten()
            .is_some()
        {
            return Ok(BrowserStatus {
                running: false,
                browser_name: Some(self.browser_name.clone()),
                target_id: self.target_id.clone(),
                title: None,
                url: None,
                profile_dir: Some(self.profile_dir.to_string_lossy().into_owned()),
            });
        }
        let target = self.ensure_page().await?;
        Ok(BrowserStatus {
            running: true,
            browser_name: Some(self.browser_name.clone()),
            target_id: Some(target.id),
            title: Some(target.title),
            url: Some(target.url),
            profile_dir: Some(self.profile_dir.to_string_lossy().into_owned()),
        })
    }

    pub async fn stop(&mut self) -> Result<(), String> {
        if let Some(extension) = &self.extension {
            // Detach only this task's debugger sessions; never close user tabs.
            let paused = if extension.live() {
                extension.pause().await
            } else {
                Ok(())
            };
            extension.close();
            paused?;
        }
        if let Some(child) = &mut self.child {
            if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                child.kill().await.map_err(|e| e.to_string())?;
            }
        }
        self.page_connection = None;
        Ok(())
    }

    pub async fn resume_extension(&self) -> Result<(), String> {
        if let Some(extension) = &self.extension {
            extension.resume().await?;
        }
        Ok(())
    }

    pub async fn navigate(&mut self, raw_url: &str) -> Result<Value, String> {
        self.active_frame = None;
        self.snapshot_pages.clear();
        self.reference_documents.clear();
        let url = validate_navigation_url(raw_url, self.allow_private_network).await?;
        let result = self
            .call_page("Page.navigate", json!({ "url": url.as_str() }))
            .await?;
        if let Some(error_text) = result.get("errorText").and_then(Value::as_str) {
            return Err(format!("浏览器导航失败：{error_text}"));
        }
        self.wait_for_ready(Duration::from_secs(20)).await?;
        let current = self.enforce_current_url_policy().await?;
        Ok(json!({ "url": current, "targetId": self.target_id }))
    }

    pub async fn current_url(&mut self) -> Result<String, String> {
        let frame = self.active_frame.take();
        let value = self.evaluate("location.href", true).await;
        self.active_frame = frame;
        let value = value?;
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "浏览器没有返回当前地址".to_string())
    }

    pub async fn snapshot(&mut self) -> Result<Value, String> {
        // Refs live on DOM nodes and remain stable until the page replaces the
        // node. Hidden/non-interactive content is omitted to keep the model
        // context bounded and focused on actionable UI.
        let saved = self.active_frame.take();
        let mut value = self.snapshot_scope().await?;
        let tree = self.call_page("Page.getFrameTree", json!({})).await?;
        fn frames(tree: &Value, ids: &mut Vec<String>) {
            if let Some(children) = tree["childFrames"].as_array() {
                for child in children {
                    if let Some(id) = child["frame"]["id"].as_str() {
                        ids.push(id.into());
                    }
                    frames(child, ids);
                }
            }
        }
        let mut ids = vec![];
        frames(&tree["frameTree"], &mut ids);
        let total = ids.len();
        let mut issues = vec![];
        for frame in ids.into_iter().take(16) {
            self.active_frame = Some(frame.clone());
            match self.snapshot_scope().await {
                Ok(mut observed) => {
                    if let Some(nodes) = observed["elements"].as_array_mut() {
                        for node in nodes {
                            node["ref"] =
                                json!(format!("{}::{}", frame, node["ref"].as_str().unwrap_or("")));
                            node["frameId"] = json!(frame);
                            value["elements"].as_array_mut().unwrap().push(node.clone());
                        }
                    }
                }
                Err(_) => issues.push(json!({"frameId":frame,"error":"frame_unavailable"})),
            }
        }
        self.active_frame = saved;
        value["frameIssues"] = json!(issues);
        value["framesTruncated"] = json!(total > 16);
        value["framesObserved"] = json!(total.min(16));
        self.bound_snapshot(value)
    }
    pub async fn snapshot_scope(&mut self) -> Result<Value, String> {
        let value = self
            .evaluate(include_str!("browser-snapshot.js"), true)
            .await?;
        self.reference_documents
            .retain(|_, (at, _, _)| at.elapsed() < Duration::from_secs(120));
        if let (Some(token), Some(nodes)) = (
            value["documentToken"].as_str(),
            value["elements"].as_array(),
        ) {
            for node in nodes {
                if let Some(reference) = node["ref"].as_str() {
                    let reference = self
                        .active_frame
                        .as_ref()
                        .map(|frame| format!("{frame}::{reference}"))
                        .unwrap_or_else(|| reference.into());
                    if self.reference_documents.len() >= 10000
                        && !self.reference_documents.contains_key(&reference)
                    {
                        return Err("reference_limit: refresh the browser task".into());
                    }
                    self.reference_documents.insert(
                        reference,
                        (
                            std::time::Instant::now(),
                            self.target_id.clone().unwrap_or_default(),
                            token.into(),
                        ),
                    );
                }
            }
        }
        Ok(value)
    }
    fn bound_snapshot(&mut self, mut value: Value) -> Result<Value, String> {
        value["resultId"] = json!(uuid::Uuid::now_v7().to_string());
        value["coverage"]=json!("bounded DOM and open shadow roots; unavailable frames and closed roots are not complete content");
        value["treeTruncated"] =
            json!(value["elements"].as_array().is_some_and(|e| e.len() >= 500));
        value["textTruncated"] = json!(value["text"]
            .as_str()
            .is_some_and(|s| s.chars().count() >= 12000));
        self.page(value, std::time::Instant::now())
    }
    fn page(&mut self, mut value: Value, created: std::time::Instant) -> Result<Value, String> {
        self.snapshot_pages
            .retain(|_, (at, _, _)| at.elapsed() < Duration::from_secs(120));
        let token = uuid::Uuid::now_v7().to_string();
        value["nextCursor"] = json!(token);
        value["pageTruncated"] = json!(true);
        let mut remaining = vec![];
        let text = value["text"].as_str().unwrap_or("").to_owned();
        let mut remaining_text = text.chars().skip(1000).collect::<String>();
        value["text"] = json!(text.chars().take(1000).collect::<String>());
        value["textContinues"] = json!(!remaining_text.is_empty());
        while serde_json::to_vec(&value).unwrap().len() > 11000 {
            let node_count = value["elements"].as_array().map_or(0, Vec::len);
            if node_count <= 1 && value["text"].as_str().is_some_and(|text| !text.is_empty()) {
                let text = value["text"].as_str().unwrap();
                let split = text.chars().count() / 2;
                remaining_text =
                    text.chars().skip(split).collect::<String>() + remaining_text.as_str();
                value["text"] = json!(text.chars().take(split).collect::<String>());
                value["textContinues"] = json!(true);
                continue;
            }
            if let Some(item) = value["elements"]
                .as_array_mut()
                .and_then(|items| items.pop())
            {
                remaining.push(item);
            } else {
                return Err("snapshot_metadata_limit: narrow the observation".into());
            }
        }
        if remaining.is_empty() && remaining_text.is_empty() {
            value["nextCursor"] = Value::Null;
            value["pageTruncated"] = json!(false);
        } else {
            if self.snapshot_pages.len() >= 32 {
                return Err("cursor_limit: consume existing browser pages".into());
            }
            remaining.reverse();
            let mut next = value.clone();
            next["elements"] = json!(remaining);
            next["text"] = json!(remaining_text);
            let bytes = self
                .snapshot_pages
                .values()
                .map(|(_, _, v)| serde_json::to_vec(v).unwrap().len())
                .sum::<usize>();
            if bytes + serde_json::to_vec(&next).unwrap().len() > 4 * 1024 * 1024 {
                return Err("cursor_limit: browser cache exhausted".into());
            }
            self.snapshot_pages.insert(
                token,
                (created, self.target_id.clone().unwrap_or_default(), next),
            );
        }
        Ok(value)
    }
    pub async fn continue_snapshot(&mut self, cursor: &str) -> Result<Value, String> {
        let (at, target, value) = self
            .snapshot_pages
            .remove(cursor)
            .ok_or("stale_cursor: unknown or consumed")?;
        if at.elapsed() > Duration::from_secs(120) || self.target_id.as_deref() != Some(&target) {
            return Err("stale_cursor: browser context changed".into());
        }
        if self.current_url().await?.as_str() != value["url"].as_str().unwrap_or("") {
            return Err("stale_cursor: page navigated".into());
        }
        let saved = self.active_frame.take();
        let current = self
            .evaluate("globalThis.__echoSnapshotDocumentToken || null", true)
            .await;
        self.active_frame = saved;
        if current? != value["documentToken"] {
            return Err("stale_cursor: document reloaded, even if URL is unchanged".into());
        }
        self.page(value, at)
    }
    pub async fn select_reference_frame(&mut self, args: &mut Value) -> Result<(), String> {
        if let Some(reference) = args["elementRef"].as_str().map(str::to_owned) {
            let (at, target, token) = self
                .reference_documents
                .get(&reference)
                .cloned()
                .ok_or("stale_reference: use a ref from this task's current snapshot")?;
            if at.elapsed() > Duration::from_secs(120) || self.target_id.as_deref() != Some(&target)
            {
                return Err("stale_reference: browser context changed".into());
            }
            if let Some((frame, reference)) = reference.split_once("::") {
                if frame.is_empty() || frame.len() > 256 || reference.is_empty() {
                    return Err("invalid_reference".into());
                }
                self.active_frame = Some(frame.into());
                args["elementRef"] = json!(reference);
            } else {
                self.active_frame = None;
            }
            if self
                .evaluate("globalThis.__echoSnapshotDocumentToken || null", true)
                .await?
                != token
            {
                return Err(
                    "stale_reference: original document reloaded; take a fresh snapshot".into(),
                );
            }
        } else if args.get("x").is_some() || args.get("fromX").is_some() {
            self.active_frame = None;
        }
        Ok(())
    }
    pub async fn wait_for(&mut self, args: &Value) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(args["timeoutMs"].as_u64().unwrap_or(5000).clamp(100, 30000));
        let state = args["state"].as_str().ok_or("state_required")?;
        loop {
            let observed = if state == "url_equals" {
                json!({"matches":self.current_url().await?.as_str()==args["text"].as_str().unwrap_or("")})
            } else {
                let reference = serde_json::to_string(
                    args["elementRef"].as_str().ok_or("elementRef_required")?,
                )
                .map_err(|e| e.to_string())?;
                self.evaluate(&format!("(()=>{{const nodes=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']');if(nodes.length>1)return {{ambiguous:true}};const e=nodes[0];if(e?.type==='password')return {{protected:true}};return {{exists:!!e,value:e?(e.value??e.innerText??''):null}};}})()"),true).await?
            };
            if observed["ambiguous"] == true || observed["protected"] == true {
                return Err("target_unavailable: ambiguous or protected browser condition".into());
            }
            let verified = match state {
                "appears" => observed["exists"] == true,
                "disappears" => observed["exists"] == false,
                "text_equals" => observed["exists"] == true && observed["value"] == args["text"],
                "url_equals" => observed["matches"] == true,
                _ => return Err("unsupported_wait_state".into()),
            };
            if verified {
                return Ok(json!({"verified":true,"status":"verified","condition":state}));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(json!({"verified":false,"status":"timeout"}));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    pub async fn screenshot(&mut self) -> Result<(String, Value), String> {
        self.active_frame = None;
        let result = self
            .call_page(
                "Page.captureScreenshot",
                json!({ "format": "png", "fromSurface": true, "captureBeyondViewport": false }),
            )
            .await?;
        let data = result
            .get("data")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "浏览器截图没有返回图像".to_string())?
            .to_string();
        let meta = self
            .evaluate(
                "({url:location.href,title:document.title,width:innerWidth,height:innerHeight,scrollX,scrollY})",
                true,
            )
            .await?;
        Ok((data, meta))
    }

    pub(super) async fn prepare_interaction(
        &mut self,
        input: Value,
    ) -> Result<BrowserInteraction, String> {
        let frames = self.call_page("Page.getFrameTree", json!({})).await?;
        let target_id = self.target_id.clone().ok_or("无法识别浏览器标签页")?;
        let frame_id = self
            .active_frame
            .as_deref()
            .or(frames["frameTree"]["frame"]["id"].as_str())
            .ok_or("无法识别浏览器文档")?
            .to_owned();
        let offset = if self.active_frame.is_some() {
            let owner = self
                .call_page("DOM.getFrameOwner", json!({"frameId":frame_id}))
                .await?;
            let model = self
                .call_page(
                    "DOM.getBoxModel",
                    json!({"backendNodeId":owner["backendNodeId"]}),
                )
                .await?;
            (
                model["model"]["content"][0]
                    .as_f64()
                    .ok_or("frame_geometry_unavailable")?,
                model["model"]["content"][1]
                    .as_f64()
                    .ok_or("frame_geometry_unavailable")?,
            )
        } else {
            (0.0, 0.0)
        };
        let world = self
            .call_page_at(
                Some(&target_id),
                "Page.createIsolatedWorld",
                json!({ "frameId": frame_id, "worldName": "EchoAgentBrowserInteraction" }),
            )
            .await?;
        let context_id = world["executionContextId"]
            .as_u64()
            .ok_or("无法识别浏览器文档执行环境")?;
        let mut approval = BrowserInteraction {
            target_id,
            context_id,
            input,
            expected: Value::Null,
            offset,
            frame_id: self.active_frame.clone(),
        };
        let token = uuid::Uuid::now_v7().to_string();
        let expected = self
            .interaction_sample(&approval, Some(&token), false, None, None)
            .await?;
        if !expected.is_object() {
            return Err("无法确认网页操作的实际目标，请重新观察页面".into());
        }
        let submission = &expected["submission"];
        if submission.is_object() && submission["method"] != "dialog" {
            let action = submission["action"]
                .as_str()
                .ok_or("无法核对表单的 HTTP/HTTPS 提交地址，请在浏览器中手动操作")?;
            validate_navigation_url(action, self.allow_private_network).await?;
        }
        let mut expected = expected;
        expected["topUrl"] = json!(self.current_url().await?);
        approval.expected = expected;
        Ok(approval)
    }

    async fn interaction_sample(
        &mut self,
        approval: &BrowserInteraction,
        new_token: Option<&str>,
        destination_only: bool,
        click_point: Option<(f64, f64)>,
        form_guard: Option<&Value>,
    ) -> Result<Value, String> {
        let input = &approval.input;
        let token = json!(new_token);
        let scroll = new_token.is_some();
        let click_point = json!(click_point.map(|(x, y)| json!({"x": x, "y": y})));
        let form_guard = json!(form_guard);
        let expression = format!(
            "({INTERACTION_TARGET_JS})({input},{token},{scroll},{destination_only},{click_point},{form_guard})"
        );
        let evaluation = self
            .call_page_at(
                Some(&approval.target_id),
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": approval.context_id,
                    "returnByValue": true,
                }),
            )
            .await?;
        if evaluation.get("exceptionDetails").is_some() {
            return Err("无法核对网页操作目标，操作已取消".into());
        }
        Ok(evaluation["result"]["value"].clone())
    }

    async fn verify_interaction(
        &mut self,
        approval: &BrowserInteraction,
        destination_only: bool,
        click_point: Option<(f64, f64)>,
        arm_form_guard: bool,
    ) -> Result<Value, String> {
        if self.current_url().await?.as_str() != approval.url() {
            return Err("target_changed: top-level page changed after approval".into());
        }
        if let Some(frame) = &approval.frame_id {
            let owner = self
                .call_page("DOM.getFrameOwner", json!({"frameId":frame}))
                .await?;
            let model = self
                .call_page(
                    "DOM.getBoxModel",
                    json!({"backendNodeId":owner["backendNodeId"]}),
                )
                .await?;
            let x = model["model"]["content"][0]
                .as_f64()
                .ok_or("frame_geometry_unavailable")?;
            let y = model["model"]["content"][1]
                .as_f64()
                .ok_or("frame_geometry_unavailable")?;
            if (x - approval.offset.0).abs() > 0.5 || (y - approval.offset.1).abs() > 0.5 {
                return Err("target_changed: iframe moved after approval; observe again".into());
            }
        }
        let submission = &approval.expected["submission"];
        let form_guard = (arm_form_guard && submission.is_object()).then_some(submission);
        let actual = self
            .interaction_sample(approval, None, destination_only, click_point, form_guard)
            .await?;
        let expected = &approval.expected;
        if actual["documentToken"] != expected["documentToken"]
            || actual["url"] != expected["url"]
            || (!destination_only && actual["target"] != expected["target"])
            || actual["destination"] != expected["destination"]
            || actual["submission"] != expected["submission"]
        {
            return Err("网页、焦点或实际命中目标已变化，操作已取消，请重新观察页面".into());
        }
        if let (Some(frame), Some(x), Some(y)) = (
            &approval.frame_id,
            actual["x"].as_f64(),
            actual["y"].as_f64(),
        ) {
            let point = self.verify_frame_point(frame, x, y).await?;
            if (point.0 - x - approval.offset.0).abs() > 0.5
                || (point.1 - y - approval.offset.1).abs() > 0.5
            {
                return Err("target_changed: iframe coordinate projection changed".into());
            }
        }
        Ok(actual)
    }

    async fn verify_frame_point(
        &mut self,
        frame: &str,
        mut x: f64,
        mut y: f64,
    ) -> Result<(f64, f64), String> {
        let tree = self.call_page("Page.getFrameTree", json!({})).await?;
        let mut parents = std::collections::HashMap::new();
        fn collect(
            tree: &Value,
            parent: Option<&str>,
            parents: &mut std::collections::HashMap<String, Option<String>>,
        ) {
            if let Some(id) = tree["frame"]["id"].as_str() {
                parents.insert(id.into(), parent.map(str::to_owned));
                if let Some(children) = tree["childFrames"].as_array() {
                    for child in children {
                        collect(child, Some(id), parents);
                    }
                }
            }
        }
        collect(&tree["frameTree"], None, &mut parents);
        let mut frame = frame.to_owned();
        for _ in 0..17 {
            let parent = parents
                .get(&frame)
                .ok_or("frame_unavailable: original frame disappeared")?;
            let Some(parent) = parent.clone() else {
                return Ok((x, y));
            };
            let owner = self
                .call_page("DOM.getFrameOwner", json!({"frameId":frame}))
                .await?;
            let resolved = self
                .call_page(
                    "DOM.resolveNode",
                    json!({"backendNodeId":owner["backendNodeId"]}),
                )
                .await?;
            let object = resolved["object"]["objectId"]
                .as_str()
                .ok_or("frame_owner_unavailable")?;
            // Verify every parent hit, including overlays outside the iframe.
            // Transformed frames are rejected rather than approximating input.
            let hit = self.call_page("Runtime.callFunctionOn", json!({"objectId":object,
                "functionDeclaration":"function(x,y){if(!this.isConnected||!['iframe','frame'].includes(this.localName))return {ok:false};for(let e=this;e;e=e.parentElement){const s=getComputedStyle(e);if(s.transform!=='none'||s.perspective!=='none'||Number(s.zoom||1)!==1)return {ok:false};}const r=this.getBoundingClientRect();x+=r.left+this.clientLeft;y+=r.top+this.clientTop;let hit=this.ownerDocument.elementFromPoint(x,y);for(let i=0;i<20&&hit?.shadowRoot;i++){const inner=hit.shadowRoot.elementFromPoint?.(x,y);if(!inner||inner===hit)break;hit=inner;}return {ok:hit===this,x,y};}",
                "arguments":[{"value":x},{"value":y}],"returnByValue":true})).await;
            let _ = self
                .call_page("Runtime.releaseObject", json!({"objectId":object}))
                .await;
            let hit = hit?;
            if hit.get("exceptionDetails").is_some() || hit["result"]["value"]["ok"] != true {
                return Err(
                    "target_changed: iframe obscured or transformed; no pointer input dispatched"
                        .into(),
                );
            }
            x = hit["result"]["value"]["x"]
                .as_f64()
                .ok_or("frame_geometry_unavailable")?;
            y = hit["result"]["value"]["y"]
                .as_f64()
                .ok_or("frame_geometry_unavailable")?;
            frame = parent;
        }
        Err("frame_limit: iframe ancestry exceeds the observation budget".into())
    }

    async fn finish_form_guard(&mut self, approval: &BrowserInteraction) -> Result<(), String> {
        if approval.expected["submission"].is_object() {
            // An allowed submission can navigate and destroy this context.
            // The guard reports a mismatch only while the original page lives.
            let result = self
                .interaction_sample(approval, None, false, None, Some(&json!("finish")))
                .await;
            if matches!(result, Ok(Value::Bool(true))) {
                return Err("表单提交目标已变化，操作已取消，请重新核对提交地址".into());
            }
        }
        Ok(())
    }

    pub(super) async fn click(
        &mut self,
        approval: &BrowserInteraction,
        button: &str,
        click_count: u32,
    ) -> Result<Value, String> {
        let button = match button {
            "left" | "right" | "middle" => button,
            _ => return Err("button 只能是 left、right 或 middle".into()),
        };
        let target_id = Some(approval.target_id.as_str());
        let actual = self
            .verify_interaction(approval, false, None, false)
            .await?;
        let (x, y) = actual["x"]
            .as_f64()
            .zip(actual["y"].as_f64())
            .ok_or("无法确定已确认控件的点击坐标")?;
        let point = Some((x, y));
        self.call_page_at(
            target_id,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x+approval.offset.0, "y": y+approval.offset.1, "button": "none" }),
        )
        .await?;
        let clicked = async {
            // Moving the pointer can reveal an overlay. Arm the form guard
            // while verifying the approved hit before sending mouse events.
            self.verify_interaction(approval, false, point, true).await?;
            self.call_page_at(
                target_id,
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": x+approval.offset.0, "y": y+approval.offset.1, "button": button, "clickCount": click_count.clamp(1, 3) }),
            )
            .await?;
            // A pressed button can move and still cover the release point.
            if let Err(error) = self.verify_interaction(approval, false, point, false).await {
                self.release_pointer_outside(approval, button).await;
                return Err(error);
            }
            let release = json!({ "type": "mouseReleased", "x": x+approval.offset.0, "y": y+approval.offset.1, "button": button, "clickCount": click_count.clamp(1, 3) });
            if let Err(error) = self.call_page_at(target_id, "Input.dispatchMouseEvent", release).await {
                self.release_pointer_outside(approval, button).await;
                return Err(error);
            }
            Ok(())
        }.await;
        let form_result = self.finish_form_guard(approval).await;
        clicked?;
        form_result?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(json!({ "clicked": true, "x": x, "y": y, "button": button }))
    }

    async fn release_pointer_outside(&mut self, approval: &BrowserInteraction, button: &str) {
        // Clear the Chromium button state without releasing over another
        // page control when the approved point has become invalid.
        let _ = self.call_page_at(
            Some(&approval.target_id),
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseReleased", "x": -1, "y": -1, "button": button, "clickCount": 0 }),
        ).await;
    }

    pub async fn hover(&mut self, element_ref: &str) -> Result<Value, String> {
        let approval = self
            .prepare_interaction(json!({"kind":"click","reference":element_ref}))
            .await?;
        let location = self
            .verify_interaction(&approval, false, None, false)
            .await?;
        let x = location
            .get("x")
            .and_then(Value::as_f64)
            .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?
            + approval.offset.0;
        let y = location
            .get("y")
            .and_then(Value::as_f64)
            .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?
            + approval.offset.1;
        self.call_page(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none" }),
        )
        .await?;
        Ok(json!({ "hovered": true, "x": x, "y": y }))
    }

    pub async fn type_text(
        &mut self,
        element_ref: &str,
        text: &str,
        replace: bool,
    ) -> Result<Value, String> {
        if text.len() > 64 * 1024 {
            return Err("单次输入不能超过 64KB".into());
        }
        let reference = serde_json::to_string(element_ref)
            .map_err(|error| format!("编码元素引用失败：{error}"))?;
        let expression = format!(
            "(() => {{ const editableText={EDITABLE_TEXT_JS}; const e=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']')[0]; if(!e) return null; if(e.matches(':disabled')||e.readOnly||e.getAttribute('aria-disabled')==='true') return {{disabled:true}}; const type=(e.type||'').toLowerCase(); if(type==='password') return {{type}}; if(!(e instanceof HTMLTextAreaElement)&&!(e instanceof HTMLInputElement)&&!e.isContentEditable) return {{unsupported:true}}; if(['checkbox','radio','file','button','submit','reset','hidden','color','range'].includes(type)) return {{unsupported:true,type}}; e.scrollIntoView({{block:'center',inline:'center'}}); e.focus(); {} return {{disabled:false,type,tag:e.tagName.toLowerCase(),editable:e.isContentEditable,before:e.isContentEditable?editableText(e):e.value}}; }})()",
            if replace {
                "if(typeof e.select==='function') e.select(); else if(e.isContentEditable) { const s=getSelection(),r=document.createRange(); r.selectNodeContents(e); s.removeAllRanges(); s.addRange(r); }"
            } else {
                ""
            }
        );
        let target = self.evaluate(&expression, true).await?;
        if target.is_null() {
            return Err("页面元素已失效，请重新获取页面快照".into());
        }
        if target.get("disabled").and_then(Value::as_bool) == Some(true) {
            return Err("目标输入框处于禁用或只读状态".into());
        }
        if target.get("type").and_then(Value::as_str) == Some("password") {
            return Err(
                "为保护凭据，代理不能接收或填写密码框。请暂停自动化并由用户接管填写。".into(),
            );
        }
        if target.get("unsupported").and_then(Value::as_bool) == Some(true) {
            return Err(
                "目标控件不支持文本输入；下拉框请用 browser_select，勾选和按钮请用 browser_click"
                    .into(),
            );
        }
        let input_type = target.get("type").and_then(Value::as_str).unwrap_or("");
        let value = serde_json::to_string(text).map_err(|e| e.to_string())?;
        if matches!(
            input_type,
            "date" | "time" | "datetime-local" | "month" | "week" | "number"
        ) {
            if !replace {
                return Err("日期、时间和数字控件需要 replace=true，并填写完整规范值".into());
            }
            // Native date editors are locale-dependent segmented controls.
            // Use their native value setter, then check the actual DOM value
            // and constraints. This is still behind the same action approval.
            self.evaluate(&format!(
                "(() => {{ const e=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']')[0]; if(!(e instanceof HTMLInputElement)||e.matches(':disabled')||e.readOnly||e.type!=={}) throw new Error('输入目标已变化'); const probe=e.cloneNode(false); probe.value={value}; if(probe.value!=={value}||!probe.checkValidity()) throw new Error('输入值不符合控件的日期、格式、范围或步长要求，原字段值保持'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,{value}); e.dispatchEvent(new Event('input',{{bubbles:true}})); e.dispatchEvent(new Event('change',{{bubbles:true}})); return true; }})()",
                serde_json::to_string(input_type).map_err(|e| e.to_string())?
            ), true).await?;
        } else {
            if text.is_empty() && replace {
                let approval = self
                    .prepare_interaction(json!({ "kind": "key", "reference": element_ref }))
                    .await?;
                self.key(&approval, "Backspace", &[]).await?;
            } else {
                self.call_page("Input.insertText", json!({ "text": text }))
                    .await?;
            }
        }
        let readback = self.evaluate(&format!(
            "(() => {{ const editableText={EDITABLE_TEXT_JS}; const e=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']')[0]; if(!e||e.type==='password') return null; return {{value:e.isContentEditable?editableText(e):e.value,valid:typeof e.checkValidity==='function'?e.checkValidity():true}}; }})()"
        ), true).await?;
        if readback.is_null() {
            return Err("输入后目标控件已变化，请重新获取页面快照检查结果，避免重复填写".into());
        }
        let actual = readback.get("value").and_then(Value::as_str).unwrap_or("");
        // Textareas and rich editors normalize CRLF to LF. The editor reader
        // preserves block/BR boundaries without counting caret placeholders.
        let rich_editor = target.get("editable").and_then(Value::as_bool) == Some(true);
        let normalize = |value: &str| {
            let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
            if rich_editor {
                normalized.replace('\u{00a0}', " ")
            } else {
                normalized
            }
        };
        if replace && normalize(actual) != normalize(text) {
            return Err("输入未按预期生效，请重新获取页面快照；日期使用 YYYY-MM-DD，时间使用 HH:mm，避免重复相同操作".into());
        }
        if !replace
            && actual == target.get("before").and_then(Value::as_str).unwrap_or("")
            && !text.is_empty()
        {
            return Err("输入后字段值没有变化，请检查目标控件".into());
        }
        if readback.get("valid").and_then(Value::as_bool) == Some(false) {
            return Err(
                "已填写，但字段未通过页面校验；请检查日期范围、长度、格式或步长后再提交".into(),
            );
        }
        Ok(json!({
            "typed": true,
            "characters": text.chars().count(),
            "verified": true,
            "valid": true,
        }))
    }

    pub async fn select(&mut self, element_ref: &str, values: &[String]) -> Result<Value, String> {
        if values.is_empty() || values.len() > 100 {
            return Err("select values 数量必须在 1 到 100 之间".into());
        }
        let reference = serde_json::to_string(element_ref)
            .map_err(|error| format!("编码元素引用失败：{error}"))?;
        let values_json =
            serde_json::to_string(values).map_err(|error| format!("编码选项失败：{error}"))?;
        let expression = format!(
            "(() => {{ const e=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']')[0]; if(!(e instanceof HTMLSelectElement)) return null; if(e.matches(':disabled')||e.getAttribute('aria-disabled')==='true') return {{error:'目标下拉框已禁用'}}; const values={values_json}; if(!e.multiple&&values.length!==1) return {{error:'单选下拉框只能选择一个值'}}; const options=Array.from(e.options); const matched=values.map(v=>options.filter(o=>!o.disabled&&!o.parentElement?.disabled&&(o.value===v||o.text===v))); if(matched.some(m=>m.length!==1)) return {{error:'选项不存在、不可用或名称不唯一，请重新获取页面快照'}}; const wanted=new Set(matched.map(m=>m[0])); for(const o of options) o.selected=wanted.has(o); e.dispatchEvent(new Event('input',{{bubbles:true}})); e.dispatchEvent(new Event('change',{{bubbles:true}})); const selected=Array.from(e.selectedOptions); return {{selected:selected.map(o=>({{value:o.value,text:o.text}})),verified:e.isConnected&&selected.length===wanted.size&&selected.every(o=>wanted.has(o)),valid:e.checkValidity()}}; }})()"
        );
        let selected = self.evaluate(&expression, true).await?;
        if selected.is_null() {
            return Err("目标不是有效的下拉选择框，或元素引用已经失效".into());
        }
        if let Some(error) = selected.get("error").and_then(Value::as_str) {
            return Err(error.into());
        }
        if selected.get("verified").and_then(Value::as_bool) != Some(true)
            || selected.get("valid").and_then(Value::as_bool) != Some(true)
        {
            return Err("选项未按预期生效或未通过页面校验，请重新检查页面".into());
        }
        Ok(selected)
    }

    pub async fn upload(
        &mut self,
        element_ref: &str,
        canonical_paths: &[String],
    ) -> Result<Value, String> {
        if canonical_paths.is_empty() || canonical_paths.len() > 20 {
            return Err("上传文件数量必须在 1 到 20 之间".into());
        }
        let reference = serde_json::to_string(element_ref).map_err(|e| e.to_string())?;
        let expression = format!("(()=>{{const nodes=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']');if(nodes.length!==1)throw Error('ambiguous_or_missing_file_input');const e=nodes[0];if(!(e instanceof HTMLInputElement)||e.type!=='file'||e.disabled)throw Error('file_input_required');return e;}})()");
        let target = self.evaluate(&expression, false).await?;
        let object = target["objectId"]
            .as_str()
            .ok_or("file_input_unavailable")?;
        let selected = self
            .call_page(
                "DOM.setFileInputFiles",
                json!({"objectId":object,"files":canonical_paths}),
            )
            .await;
        let _ = self
            .call_page("Runtime.releaseObject", json!({"objectId":object}))
            .await;
        selected?;
        let actual = self.evaluate(&format!("(()=>{{const nodes=globalThis.__echoQueryAll('[data-echo-ref='+CSS.escape({reference})+']');return nodes.length===1?Array.from(nodes[0].files||[]).map(f=>f.name):null;}})()"), true).await?;
        let expected: Vec<_> = canonical_paths
            .iter()
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        if actual != json!(expected) {
            return Err(
                "result_unknown: selected files could not be verified; observe before retry".into(),
            );
        }
        Ok(
            json!({ "uploaded": true, "fileCount": canonical_paths.len(), "verified": true, "status":"files_selected_unsubmitted", "uploadConfirmed":false }),
        )
    }

    pub fn downloads(&self) -> Result<Vec<BrowserDownload>, String> {
        if self.extension.is_some() {
            return Err("unsupported_action: extension downloads stay in the user's browser; verify the explicit output path instead".into());
        }
        let directory = self.download_dir();
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("创建浏览器下载目录失败：{error}"))?;
        crate::paths::harden_private_dir(&directory)?;
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("读取浏览器下载目录失败：{error}"))?;
        let mut downloads = Vec::new();
        for entry in entries.flatten().take(200) {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let complete = !name.ends_with(".crdownload") && !name.ends_with(".tmp");
            let modified_at_ms = metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| value.as_millis().min(u128::from(u64::MAX)) as u64)
                .unwrap_or_default();
            downloads.push(BrowserDownload {
                name,
                path: entry.path().to_string_lossy().into_owned(),
                bytes: metadata.len(),
                modified_at_ms,
                complete,
            });
        }
        downloads.sort_by_key(|download| std::cmp::Reverse(download.modified_at_ms));
        downloads.truncate(100);
        Ok(downloads)
    }

    pub(super) async fn key(
        &mut self,
        approval: &BrowserInteraction,
        key: &str,
        modifiers: &[String],
    ) -> Result<Value, String> {
        if key.is_empty() || key.chars().count() > 64 {
            return Err("按键名称为空或过长".into());
        }
        let modifiers_value = cdp_modifiers(modifiers)?;
        let (key_value, code, windows_virtual_key_code) = normalize_cdp_key(key);
        let base = json!({
            "key": key_value,
            "code": code,
            "windowsVirtualKeyCode": windows_virtual_key_code,
            "nativeVirtualKeyCode": windows_virtual_key_code,
            "modifiers": modifiers_value,
        });
        let target_id = Some(approval.target_id.as_str());
        let pressed = async {
            self.verify_interaction(approval, false, None, true).await?;
            let mut down = base.clone();
            down["type"] = Value::String("keyDown".into());
            self.call_page_at(target_id, "Input.dispatchKeyEvent", down)
                .await?;
            let mut up = base;
            up["type"] = Value::String("keyUp".into());
            if let Err(error) = self
                .call_page_at(target_id, "Input.dispatchKeyEvent", up.clone())
                .await
            {
                let _ = self
                    .call_page_at(target_id, "Input.dispatchKeyEvent", up)
                    .await;
                return Err(error);
            }
            Ok(())
        }
        .await;
        let form_result = self.finish_form_guard(approval).await;
        pressed?;
        form_result?;
        Ok(json!({ "pressed": key, "modifiers": modifiers }))
    }

    pub async fn scroll(&mut self, delta_x: f64, delta_y: f64) -> Result<Value, String> {
        self.active_frame = None;
        let viewport = self
            .evaluate("({x:innerWidth/2,y:innerHeight/2})", true)
            .await?;
        let x = viewport.get("x").and_then(Value::as_f64).unwrap_or(500.0);
        let y = viewport.get("y").and_then(Value::as_f64).unwrap_or(400.0);
        self.call_page(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x.clamp(-10000.0, 10000.0), "deltaY": delta_y.clamp(-10000.0, 10000.0) }),
        )
        .await?;
        Ok(json!({ "scrolled": true, "deltaX": delta_x, "deltaY": delta_y }))
    }

    pub(super) async fn drag(
        &mut self,
        approval: &BrowserInteraction,
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
    ) -> Result<Value, String> {
        self.verify_interaction(approval, false, None, false)
            .await?;
        let target_id = Some(approval.target_id.as_str());
        self.call_page_at(
            target_id,
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": from_x, "y": from_y }),
        )
        .await?;
        self.verify_interaction(approval, false, None, false)
            .await?;
        self.call_page_at(
            target_id,
            "Input.dispatchMouseEvent",
            json!({ "type": "mousePressed", "x": from_x, "y": from_y, "button": "left", "clickCount": 1 }),
        )
        .await?;
        let movement = async {
            for step in 1..=8 {
                let fraction = f64::from(step) / 8.0;
                let x = from_x + (to_x - from_x) * fraction;
                let y = from_y + (to_y - from_y) * fraction;
                self.call_page_at(
                    target_id,
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseMoved", "x": x, "y": y, "button": "left", "buttons": 1 }),
                )
                .await?;
            }
            Ok::<(), String>(())
        }
        .await;
        if let Err(error) = movement {
            self.release_pointer_outside(approval, "left").await;
            return Err(error);
        }
        if let Err(error) = self.verify_interaction(approval, true, None, false).await {
            self.release_pointer_outside(approval, "left").await;
            return Err(error);
        }
        let release_event = json!({ "type": "mouseReleased", "x": to_x, "y": to_y, "button": "left", "clickCount": 1 });
        if let Err(error) = self
            .call_page_at(target_id, "Input.dispatchMouseEvent", release_event)
            .await
        {
            self.release_pointer_outside(approval, "left").await;
            return Err(error);
        }
        Ok(json!({ "dragged": true, "from": [from_x, from_y], "to": [to_x, to_y] }))
    }

    pub async fn tabs(&mut self) -> Result<Vec<BrowserTarget>, String> {
        self.targets().await
    }

    pub async fn new_tab(&mut self, raw_url: Option<&str>) -> Result<BrowserTarget, String> {
        self.reset_observation_context();
        if let Some(extension) = &self.extension {
            let url = raw_url.unwrap_or("about:blank");
            validate_navigation_url(url, self.allow_private_network).await?;
            let target: BrowserTarget = serde_json::from_value(
                extension
                    .call("Echo.newTab", json!({"url":url}), None)
                    .await?,
            )
            .map_err(|e| e.to_string())?;
            self.target_id = Some(target.id.clone());
            return Ok(target);
        }
        let url = match raw_url {
            Some(value) => validate_navigation_url(value, self.allow_private_network)
                .await?
                .to_string(),
            None => "about:blank".to_string(),
        };
        let endpoint = format!(
            "http://127.0.0.1:{}/json/new?{}",
            self.port,
            urlencoding::encode(&url)
        );
        let target = control_client()?
            .put(endpoint)
            .send()
            .await
            .map_err(|error| format!("创建浏览器标签页失败：{error}"))?
            .error_for_status()
            .map_err(|error| format!("创建浏览器标签页失败：{error}"))?
            .json::<BrowserTarget>()
            .await
            .map_err(|error| format!("解析浏览器标签页失败：{error}"))?;
        self.target_id = Some(target.id.clone());
        Ok(target)
    }

    pub async fn select_tab(&mut self, target_id: &str) -> Result<BrowserTarget, String> {
        self.reset_observation_context();
        if let Some(extension) = &self.extension {
            let target: BrowserTarget = serde_json::from_value(
                extension
                    .call("Echo.selectTab", json!({}), Some(target_id))
                    .await?,
            )
            .map_err(|e| e.to_string())?;
            self.target_id = Some(target.id.clone());
            return Ok(target);
        }
        let target = self
            .targets()
            .await?
            .into_iter()
            .find(|target| target.id == target_id && target.target_type == "page")
            .ok_or_else(|| "指定的浏览器标签页不存在".to_string())?;
        self.target_id = Some(target.id.clone());
        let endpoint = format!("http://127.0.0.1:{}/json/activate/{}", self.port, target.id);
        control_client()?
            .get(endpoint)
            .send()
            .await
            .map_err(|error| format!("切换浏览器标签页失败：{error}"))?
            .error_for_status()
            .map_err(|error| format!("切换浏览器标签页失败：{error}"))?;
        Ok(target)
    }

    pub async fn close_tab(&mut self, target_id: &str) -> Result<Value, String> {
        if let Some(extension) = &self.extension {
            let result = extension
                .call("Echo.closeTab", json!({}), Some(target_id))
                .await?;
            if self.target_id.as_deref() == Some(target_id) {
                self.target_id = None;
                self.reset_observation_context();
            }
            return Ok(result);
        }
        let endpoint = format!("http://127.0.0.1:{}/json/close/{target_id}", self.port);
        control_client()?
            .get(endpoint)
            .send()
            .await
            .map_err(|error| format!("关闭浏览器标签页失败：{error}"))?
            .error_for_status()
            .map_err(|error| format!("关闭浏览器标签页失败：{error}"))?;
        if self.target_id.as_deref() == Some(target_id) {
            self.reset_observation_context();
            self.target_id = None;
            let replacement = self.ensure_page().await?;
            self.target_id = Some(replacement.id);
        }
        Ok(json!({ "closed": target_id, "activeTargetId": self.target_id }))
    }

    pub async fn wait(&mut self, milliseconds: u64) -> Result<Value, String> {
        let bounded = milliseconds.clamp(50, 30_000);
        tokio::time::sleep(Duration::from_millis(bounded)).await;
        Ok(json!({ "waitedMs": bounded, "url": self.current_url().await? }))
    }

    fn reset_observation_context(&mut self) {
        self.active_frame = None;
        self.snapshot_pages.clear();
        self.reference_documents.clear();
    }

    fn download_dir(&self) -> PathBuf {
        self.profile_dir.join("Downloads")
    }

    async fn evaluate(&mut self, expression: &str, return_by_value: bool) -> Result<Value, String> {
        let expression = format!("globalThis.__echoQueryAll=({DEEP_QUERY_JS});{expression}");
        let mut params = json!({"expression":expression,"returnByValue":return_by_value,"awaitPromise":true,"userGesture":true});
        let frame = if let Some(frame) = self.active_frame.clone() {
            frame
        } else {
            let tree = self.call_page("Page.getFrameTree", json!({})).await?;
            tree["frameTree"]["frame"]["id"]
                .as_str()
                .ok_or("frame_unavailable")?
                .to_owned()
        };
        let world = self
            .call_page(
                "Page.createIsolatedWorld",
                json!({"frameId":frame,"worldName":"EchoAgentBrowserInteraction"}),
            )
            .await?;
        params["contextId"] = world["executionContextId"].clone();
        let result = self.call_page("Runtime.evaluate", params).await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(format!(
                "页面脚本执行失败：{}",
                compact_json(exception, 2_000)
            ));
        }
        if !return_by_value {
            return Ok(result["result"].clone());
        }
        Ok(result
            .get("result")
            .and_then(|value| value.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    async fn wait_for_ready(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let state = self.evaluate("document.readyState", true).await?;
            if matches!(state.as_str(), Some("interactive" | "complete")) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("等待页面加载完成超时".into());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn call_page(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.call_page_at(None, method, params).await
    }

    async fn call_page_at(
        &mut self,
        expected_target: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        if let Some(extension) = &self.extension {
            if let Some(target) = expected_target {
                if self.target_id.as_deref() != Some(target) {
                    return Err("browser_target_changed".into());
                }
            }
            return extension
                .call(
                    method,
                    params,
                    expected_target.or(self.target_id.as_deref()),
                )
                .await;
        }
        let future = async {
            // Target discovery is part of the command deadline. Otherwise a
            // wedged local DevTools HTTP endpoint could hold the browser mutex
            // forever and make pause/stop/status appear frozen.
            let target = if let Some(expected) = expected_target {
                if self.target_id.as_deref() != Some(expected) {
                    return Err("浏览器标签页已变化，操作已取消，请重新观察页面".into());
                }
                self.targets()
                    .await?
                    .into_iter()
                    .find(|target| {
                        target.id == expected && !target.websocket_debugger_url.is_empty()
                    })
                    .ok_or("浏览器标签页已关闭，操作已取消")?
            } else {
                self.ensure_page().await?
            };
            let id = self.next_cdp_id;
            self.next_cdp_id = self.next_cdp_id.saturating_add(1);
            // Check the retained session before every page action: if its
            // override has vanished, fail before another download can escape
            // to the system directory. A fresh browser reconnect restores it.
            let download_session = self
                .download_session
                .as_mut()
                .ok_or_else(|| "浏览器下载隔离连接已失效，请重新启动受控浏览器".to_string())?;
            cdp_request(download_session, id, "Browser.getVersion", json!({}))
                .await
                .map_err(|error| {
                    format!("浏览器下载隔离连接已失效，请重新启动受控浏览器：{error}")
                })?;
            if self
                .page_connection
                .as_ref()
                .is_none_or(|(id, _)| id != &target.id)
            {
                let (socket, _) = tokio_tungstenite::connect_async(&target.websocket_debugger_url)
                    .await
                    .map_err(|e| format!("连接受控浏览器失败：{e}"))?;
                self.page_connection = Some((target.id.clone(), socket));
            }
            let result = cdp_request(
                &mut self.page_connection.as_mut().unwrap().1,
                id,
                method,
                params,
            )
            .await;
            if result.is_err() {
                self.page_connection = None;
            }
            result
        };
        tokio::time::timeout(COMMAND_TIMEOUT, future)
            .await
            .map_err(|_| format!("浏览器命令 {method} 执行超时"))?
    }

    async fn ensure_page(&mut self) -> Result<BrowserTarget, String> {
        let targets = self.targets().await?;
        if let Some(current) = self
            .target_id
            .as_deref()
            .and_then(|id| targets.iter().find(|target| target.id == id))
            .cloned()
        {
            if !current.websocket_debugger_url.is_empty() {
                return Ok(current);
            }
        }
        if let Some(target) = targets.into_iter().find(|target| {
            target.target_type == "page" && !target.websocket_debugger_url.is_empty()
        }) {
            self.target_id = Some(target.id.clone());
            return Ok(target);
        }
        self.new_tab(None).await
    }

    async fn targets(&self) -> Result<Vec<BrowserTarget>, String> {
        if let Some(extension) = &self.extension {
            return serde_json::from_value(extension.call("Echo.tabs", json!({}), None).await?)
                .map_err(|e| e.to_string());
        }
        let endpoint = format!("http://127.0.0.1:{}/json/list", self.port);
        control_client()?
            .get(endpoint)
            .send()
            .await
            .map_err(|error| format!("读取浏览器标签页失败：{error}"))?
            .error_for_status()
            .map_err(|error| format!("读取浏览器标签页失败：{error}"))?
            .json::<Vec<BrowserTarget>>()
            .await
            .map_err(|error| format!("解析浏览器标签页失败：{error}"))
    }
}

async fn cdp_request(
    websocket: &mut CdpConnection,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let request = json!({ "id": id, "method": method, "params": params });
    websocket
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(|error| format!("发送浏览器命令失败：{error}"))?;
    while let Some(message) = websocket.next().await {
        let message = message.map_err(|error| format!("读取浏览器响应失败：{error}"))?;
        let Message::Text(text) = message else {
            continue;
        };
        if text.len() > MAX_CDP_MESSAGE_BYTES {
            return Err("浏览器响应超过 16MB 安全上限".into());
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|error| format!("解析浏览器响应失败：{error}"))?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(format!(
                "浏览器命令 {method} 失败：{}",
                compact_json(error, 2_000)
            ));
        }
        return Ok(value.get("result").cloned().unwrap_or_else(|| json!({})));
    }
    Err("受控浏览器在返回结果前断开连接".into())
}

fn control_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| format!("创建浏览器本地控制连接失败：{error}"))
}

pub fn capability() -> BrowserCapability {
    match discover_browser() {
        Some((name, executable)) => BrowserCapability {
            available: true,
            browser_name: Some(name),
            executable: Some(executable.to_string_lossy().into_owned()),
            reason: None,
        },
        None => BrowserCapability {
            available: false,
            browser_name: None,
            executable: None,
            reason: Some(
                "未找到 Google Chrome、Microsoft Edge 或 Chromium。安装后即可使用受控浏览器。"
                    .into(),
            ),
        },
    }
}

fn profile_dir(session_id: &str) -> Result<PathBuf, String> {
    if session_id.is_empty()
        || session_id.chars().count() > 256
        || session_id.chars().any(char::is_control)
    {
        return Err("自动化会话 ID 无效".into());
    }
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    Ok(crate::paths::echo_agent_home_dir()
        .join("automation")
        .join("browser-profiles")
        .join(&digest[..32]))
}

pub async fn remove_profile(session_id: &str) -> Result<(), String> {
    let path = profile_dir(session_id)?;
    let metadata = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("检查受控浏览器数据失败：{error}")),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|error| format!("清理受控浏览器数据失败：{error}"))
    } else {
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|error| format!("清理受控浏览器数据失败：{error}"))
    }
}

pub fn profile_exists(session_id: &str) -> bool {
    profile_dir(session_id).is_ok_and(|path| path.is_dir())
}

async fn wait_for_debug_port(path: &Path, timeout: Duration) -> Result<u16, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => {
                let port = contents
                    .lines()
                    .next()
                    .and_then(|line| line.trim().parse::<u16>().ok())
                    .filter(|port| *port > 0)
                    .ok_or_else(|| "浏览器调试端口文件内容无效".to_string())?;
                return Ok(port);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("读取浏览器调试端口失败：{error}")),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("浏览器启动超时，未能建立受控连接".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn discover_browser() -> Option<(String, PathBuf)> {
    browser_candidates()
        .into_iter()
        .find(|(_, path)| path.is_file())
}

#[cfg(target_os = "macos")]
fn browser_candidates() -> Vec<(String, PathBuf)> {
    let mut candidates = vec![
        (
            "Google Chrome".into(),
            PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
        ),
        (
            "Microsoft Edge".into(),
            PathBuf::from("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
        ),
        (
            "Chromium".into(),
            PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        ),
        (
            "Google Chrome Canary".into(),
            PathBuf::from(
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            ),
        ),
    ];
    if let Some(home) = dirs::home_dir() {
        candidates.extend([
            (
                "Google Chrome".into(),
                home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
            ),
            (
                "Microsoft Edge".into(),
                home.join("Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            ),
            (
                "Chromium".into(),
                home.join("Applications/Chromium.app/Contents/MacOS/Chromium"),
            ),
        ]);
    }
    candidates
}

#[cfg(target_os = "windows")]
fn browser_candidates() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    for name in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
        if let Some(value) = std::env::var_os(name) {
            roots.push(PathBuf::from(value));
        }
    }
    let mut candidates = Vec::new();
    for root in roots {
        candidates.extend([
            (
                "Microsoft Edge".into(),
                root.join("Microsoft/Edge/Application/msedge.exe"),
            ),
            (
                "Google Chrome".into(),
                root.join("Google/Chrome/Application/chrome.exe"),
            ),
            (
                "Chromium".into(),
                root.join("Chromium/Application/chrome.exe"),
            ),
        ]);
    }
    candidates
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn browser_candidates() -> Vec<(String, PathBuf)> {
    [
        ("Google Chrome", "/usr/bin/google-chrome"),
        ("Chromium", "/usr/bin/chromium"),
        ("Chromium", "/usr/bin/chromium-browser"),
        ("Microsoft Edge", "/usr/bin/microsoft-edge"),
    ]
    .into_iter()
    .map(|(name, path)| (name.to_string(), PathBuf::from(path)))
    .collect()
}

async fn validate_navigation_url(raw: &str, allow_private_network: bool) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("about:blank") {
        return Url::parse("about:blank").map_err(|error| error.to_string());
    }
    let url = Url::parse(trimmed)
        .map_err(|_| "网址格式无效，请提供完整的 http/https 地址".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("受控浏览器仅允许 http/https 地址".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("网址中不能包含明文账号或密码".into());
    }
    let host = url.host_str().ok_or_else(|| "网址缺少主机名".to_string())?;
    if host.len() > 253 || host.chars().any(char::is_control) {
        return Err("网址主机名无效或过长".into());
    }
    if !allow_private_network {
        let port = url.port_or_known_default().unwrap_or(443);
        resolve_allowed_destination(host, port, false)
            .await
            .map_err(|_| {
                "默认禁止访问本机或内网地址；请由用户在“设置 > 安全与权限 > 浏览器数据与访问范围”中为当前任务显式授权".to_string()
            })?;
    }
    Ok(url)
}

fn cdp_modifiers(modifiers: &[String]) -> Result<u8, String> {
    let mut value = 0_u8;
    for modifier in modifiers {
        value |= match modifier.to_ascii_lowercase().as_str() {
            "alt" | "option" => 1,
            "control" | "ctrl" => 2,
            "meta" | "command" | "cmd" => 4,
            "shift" => 8,
            other => return Err(format!("不支持的修饰键：{other}")),
        };
    }
    Ok(value)
}

fn normalize_cdp_key(key: &str) -> (String, String, u32) {
    match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter".into(), "Enter".into(), 13),
        "tab" => ("Tab".into(), "Tab".into(), 9),
        "escape" | "esc" => ("Escape".into(), "Escape".into(), 27),
        "backspace" => ("Backspace".into(), "Backspace".into(), 8),
        "delete" => ("Delete".into(), "Delete".into(), 46),
        "arrowup" | "up" => ("ArrowUp".into(), "ArrowUp".into(), 38),
        "arrowdown" | "down" => ("ArrowDown".into(), "ArrowDown".into(), 40),
        "arrowleft" | "left" => ("ArrowLeft".into(), "ArrowLeft".into(), 37),
        "arrowright" | "right" => ("ArrowRight".into(), "ArrowRight".into(), 39),
        "home" => ("Home".into(), "Home".into(), 36),
        "end" => ("End".into(), "End".into(), 35),
        "pageup" => ("PageUp".into(), "PageUp".into(), 33),
        "pagedown" => ("PageDown".into(), "PageDown".into(), 34),
        "space" => (" ".into(), "Space".into(), 32),
        value => {
            let first = value.chars().next().unwrap_or_default();
            let upper = first.to_ascii_uppercase().to_string();
            (
                key.to_string(),
                format!("Key{upper}"),
                u32::from(first.to_ascii_uppercase()),
            )
        }
    }
}

fn compact_json(value: &Value, max_bytes: usize) -> String {
    let text = value.to_string();
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paginated_snapshot_keeps_controls_on_the_first_page_and_preserves_all_text() {
        let mut browser = BrowserController {
            child: None,
            extension: None,
            page_connection: None,
            active_frame: None,
            snapshot_pages: Default::default(),
            reference_documents: Default::default(),
            _protection: None,
            port: 0,
            browser_name: "synthetic".into(),
            profile_dir: PathBuf::from("synthetic-not-opened"),
            target_id: Some("synthetic".into()),
            allow_private_network: false,
            network_proxy: None,
            next_cdp_id: 1,
            download_session: None,
        };
        let text = "中文正文\n".repeat(1800);
        let nodes: Vec<_> = (0..500)
            .map(|i| json!({"ref":format!("e{i}"),"name":"控制内容".repeat(40)}))
            .collect();
        let mut page = browser.bound_snapshot(json!({"url":"https://example.invalid/","documentToken":"synthetic","text":text,"elements":nodes})).unwrap();
        assert!(!page["elements"].as_array().unwrap().is_empty());
        let mut actual_text = String::new();
        let mut refs = vec![];
        for _ in 0..512 {
            assert!(serde_json::to_vec(&page).unwrap().len() <= 11000);
            actual_text.push_str(page["text"].as_str().unwrap());
            refs.extend(
                page["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|node| node["ref"].as_str().unwrap().to_owned()),
            );
            let Some(cursor) = page["nextCursor"].as_str() else {
                break;
            };
            let (created, _, next) = browser.snapshot_pages.remove(cursor).unwrap();
            page = browser.page(next, created).unwrap();
        }
        assert!(page["nextCursor"].is_null());
        assert_eq!(actual_text, text);
        assert_eq!(refs, (0..500).map(|i| format!("e{i}")).collect::<Vec<_>>());
    }
    use crate::automation::network_proxy::is_private_or_local;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn private_ip_policy_is_fail_closed() {
        for ip in [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("fd00::1".parse().unwrap()),
            IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
        ] {
            assert!(is_private_or_local(ip), "{ip} must be blocked");
        }
        assert!(!is_private_or_local(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn cdp_modifier_mapping_is_stable() {
        assert_eq!(cdp_modifiers(&["cmd".into(), "shift".into()]).unwrap(), 12);
        assert!(cdp_modifiers(&["hyper".into()]).is_err());
    }

    #[tokio::test]
    #[ignore = "requires isolated ECHO_AGENT_HOME and Chromium; supports ECHO_BROWSER_TEST_HEADLESS=1"]
    async fn controlled_browser_frames_shadow_refs_and_upload() {
        assert!(crate::paths::redirected_data_home().is_some());
        let session = format!("browser-frames-{}", uuid::Uuid::now_v7());
        let mut browser = BrowserController::launch(&session, false).await.unwrap();
        browser.evaluate(r#"(()=>{
          document.body.innerHTML='<span id="shadow-label">Wrong outer label</span><div id="shadow"></div><iframe id="child" style="width:500px;height:250px"></iframe>';
          document.querySelector('#shadow').attachShadow({mode:'open'}).innerHTML='<span id="shadow-label">Shadow text</span><input aria-labelledby="shadow-label">';
          const child=document.querySelector('#child');child.srcdoc='<button onclick="document.body.dataset.clicked=1">Inside frame</button><input type="file" aria-label="Frame file">';
          return true;
        })()"#,true).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let snapshot = browser.snapshot().await.unwrap();
        let elements = snapshot["elements"].as_array().unwrap();
        let reference = |name: &str| {
            elements.iter().find(|e| e["name"] == name).unwrap()["ref"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let shadow = reference("Shadow text");
        let button = reference("Inside frame");
        let file = reference("Frame file");
        assert!(button.contains("::"));
        let mut args = json!({"elementRef":shadow});
        browser.select_reference_frame(&mut args).await.unwrap();
        assert_eq!(
            browser
                .type_text(args["elementRef"].as_str().unwrap(), "中文 shadow", true)
                .await
                .unwrap()["verified"],
            true
        );
        let mut args = json!({"elementRef":button});
        browser.select_reference_frame(&mut args).await.unwrap();
        let approval = browser
            .prepare_interaction(json!({"kind":"click","reference":args["elementRef"]}))
            .await
            .unwrap();
        let frame = browser.active_frame.take();
        browser.evaluate("document.body.insertAdjacentHTML('beforeend','<div id=cover style=\"position:fixed;inset:0;z-index:9999\"></div>');true",true).await.unwrap();
        browser.active_frame = frame;
        assert!(
            browser.click(&approval, "left", 1).await.is_err(),
            "parent overlay must block frame input"
        );
        let frame = browser.active_frame.take();
        browser
            .evaluate("document.querySelector('#cover').remove();true", true)
            .await
            .unwrap();
        browser.active_frame = frame;
        let approval = browser
            .prepare_interaction(json!({"kind":"click","reference":args["elementRef"]}))
            .await
            .unwrap();
        browser.click(&approval, "left", 1).await.unwrap();
        assert_eq!(
            browser
                .evaluate("document.body.dataset.clicked", true)
                .await
                .unwrap(),
            "1"
        );
        let mut args = json!({"elementRef":file});
        browser.select_reference_frame(&mut args).await.unwrap();
        let output = browser.profile_dir.join("synthetic.txt");
        std::fs::write(&output, "合成上传文本").unwrap();
        assert_eq!(
            browser
                .upload(
                    args["elementRef"].as_str().unwrap(),
                    &[output.to_string_lossy().into_owned()]
                )
                .await
                .unwrap()["uploadConfirmed"],
            false
        );
        browser.active_frame = None;
        browser.evaluate("for(let i=0;i<150;i++){const e=document.createElement('button');e.textContent='Long button '+i+' '+ 'x'.repeat(100);document.body.append(e)};true",true).await.unwrap();
        let snapshot = browser.snapshot().await.unwrap();
        let cursor = snapshot["nextCursor"].as_str().unwrap().to_owned();
        let old = snapshot["elements"][0]["ref"].as_str().unwrap().to_owned();
        let _ = browser.evaluate("location.reload()", true).await;
        browser
            .wait_for_ready(Duration::from_secs(5))
            .await
            .unwrap();
        assert!(browser.continue_snapshot(&cursor).await.is_err());
        assert!(browser
            .select_reference_frame(&mut json!({"elementRef":old}))
            .await
            .is_err());
        browser.stop().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires isolated ECHO_AGENT_HOME, Chromium and desktop session"]
    async fn controlled_browser_rejects_changed_approved_interactions() {
        assert!(crate::paths::redirected_data_home().is_some());
        let session = format!("browser-approval-smoke-{}", uuid::Uuid::now_v7());
        let mut browser = BrowserController::launch(&session, false).await.unwrap();
        browser.evaluate(r#"(() => {
          document.body.innerHTML='<button style="position:absolute;top:20px;left:20px;width:100px;height:40px"><span>Approved</span></button><button style="position:absolute;top:20px;left:200px;width:100px;height:40px">Other</button>';
          const style=document.createElement('style');
          style.textContent='button:active { transform:translateY(1px) }';
          document.head.append(style);
          globalThis.hits=[];
          globalThis.installCover=()=> {
            const cover=document.createElement('button');
            cover.id='cover';
            cover.textContent='Not approved';
            cover.style='position:absolute;top:20px;left:20px;width:100px;height:40px;z-index:100';
            document.body.append(cover);
          };
          for (const type of ['mousedown','keydown','click']) {
            document.addEventListener(type, event=>hits.push([type,event.target.textContent]));
          }
          return true;
        })()"#, true).await.unwrap();
        let snapshot = browser.snapshot().await.unwrap();
        let reference = snapshot["elements"][0]["ref"].as_str().unwrap();
        let capture_click = json!({ "kind": "click", "reference": reference });
        let approved = browser
            .prepare_interaction(capture_click.clone())
            .await
            .unwrap();
        assert_eq!(approved.target()["name"], "Approved");
        browser.evaluate("installCover()", true).await.unwrap();
        assert!(browser.click(&approved, "left", 1).await.is_err());
        browser
            .evaluate("document.querySelector('#cover').remove()", true)
            .await
            .unwrap();
        browser.click(&approved, "left", 1).await.unwrap();
        assert_eq!(
            browser.evaluate("hits", true).await.unwrap(),
            json!([["mousedown", "Approved"], ["click", "Approved"]])
        );

        browser
            .evaluate("hits=[]; document.querySelector('button').focus()", true)
            .await
            .unwrap();
        let key = browser
            .prepare_interaction(json!({"kind": "key", "key": "Enter"}))
            .await
            .unwrap();
        browser
            .evaluate("document.querySelectorAll('button')[1].focus()", true)
            .await
            .unwrap();
        assert!(browser.key(&key, "Enter", &[]).await.is_err());

        let coords = browser
            .prepare_interaction(json!({
                "kind": "click", "reference": null, "x": 70, "y": 40
            }))
            .await
            .unwrap();
        browser.evaluate("document.querySelector('button').replaceWith(document.querySelector('button').cloneNode(true))", true).await.unwrap();
        assert!(browser.click(&coords, "left", 1).await.is_err());

        let drag = browser
            .prepare_interaction(json!({
                "kind": "drag", "fromX": 70, "fromY": 40, "toX": 250, "toY": 40
            }))
            .await
            .unwrap();
        assert_eq!(drag.destination()["name"], "Other");
        browser.evaluate("document.querySelectorAll('button')[1].replaceWith(document.querySelectorAll('button')[1].cloneNode(true))", true).await.unwrap();
        assert!(browser.drag(&drag, 70.0, 40.0, 250.0, 40.0).await.is_err());

        let click = browser.prepare_interaction(capture_click).await.unwrap();
        browser.evaluate("document.querySelector('button').addEventListener('mouseover', installCover, {once:true}); document.querySelector('button').blur()", true).await.unwrap();
        browser.scroll(0.0, 0.0).await.unwrap();
        browser
            .call_page(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseMoved", "x": 400, "y": 100}),
            )
            .await
            .unwrap();
        assert!(browser.click(&click, "left", 1).await.is_err());
        assert_eq!(browser.evaluate("hits", true).await.unwrap(), json!([]));

        browser
            .evaluate("document.querySelector('#cover').remove()", true)
            .await
            .unwrap();
        let click = browser
            .prepare_interaction(json!({
                "kind": "click", "reference": null, "x": 70, "y": 40
            }))
            .await
            .unwrap();
        browser.new_tab(None).await.unwrap();
        assert!(browser.click(&click, "left", 1).await.is_err());

        browser.stop().await.unwrap();
        remove_profile(&session).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires isolated ECHO_AGENT_HOME, Chromium and desktop session"]
    async fn controlled_browser_download_stays_in_task_profile_across_tabs() {
        assert!(crate::paths::redirected_data_home().is_some());
        assert!(capability().available);
        let session_id = format!("browser-download-smoke-{}", uuid::Uuid::now_v7());
        let mut browser = BrowserController::launch(&session_id, false)
            .await
            .expect("launch controlled browser");
        let expected = "中文任务下载\n仅本次隔离目录\n";
        let attempt = async {
            for index in 0..2 {
                if index == 1 {
                    browser.new_tab(None).await.expect("second page");
                }
                let name = format!("echo-download-{}-{index}.txt", uuid::Uuid::now_v7());
                let expression = format!(
                    "(() => {{ const a=document.createElement('a'); a.textContent='下载本次文件'; a.href=URL.createObjectURL(new Blob([{}],{{type:'text/plain;charset=utf-8'}})); a.download={}; document.body.replaceChildren(a); return true; }})()",
                    serde_json::to_string(expected).unwrap(),
                    serde_json::to_string(&name).unwrap()
                );
                browser
                    .evaluate(&expression, true)
                    .await
                    .expect("own blob page");
                let snapshot = browser.snapshot().await.expect("snapshot");
                let reference = snapshot["elements"].as_array().unwrap()[0]["ref"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let approval = browser
                    .prepare_interaction(json!({ "kind": "click", "reference": reference }))
                    .await
                    .expect("capture download target");
                browser
                    .click(&approval, "left", 1)
                    .await
                    .expect("download click");
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                let download = loop {
                    if let Some(download) = browser
                        .downloads()
                        .unwrap()
                        .into_iter()
                        .find(|download| download.name == name && download.complete)
                    {
                        break download;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "download absent from task profile after real click: {name}"
                    );
                    tokio::time::sleep(Duration::from_millis(100)).await;
                };
                assert_eq!(
                    PathBuf::from(&download.path),
                    browser.download_dir().join(&name)
                );
                assert_eq!(std::fs::read(&download.path).unwrap(), expected.as_bytes());
            }
        };
        // A failed assertion still drops/kills the owned browser; this test does
        // not delete or inspect unrelated files in the user's Downloads folder.
        attempt.await;
        browser
            .download_session
            .as_mut()
            .expect("retained download session")
            .close(None)
            .await
            .expect("disconnect owned download session");
        let error = browser
            .snapshot()
            .await
            .expect_err("lost isolation must block page commands");
        assert!(error.contains("下载隔离连接已失效"));
        browser.stop().await.expect("stop browser");
        remove_profile(&session_id)
            .await
            .expect("remove owned profile");
    }

    #[tokio::test]
    #[ignore = "requires isolated ECHO_AGENT_HOME, Chromium and desktop session"]
    async fn controlled_browser_smoke_covers_observe_and_interact() {
        assert!(
            crate::paths::redirected_data_home().is_some(),
            "Set ECHO_AGENT_HOME to an isolated directory before running this smoke test"
        );
        let support = capability();
        assert!(
            support.available,
            "Browser smoke requires Chromium: {:?}",
            support.reason
        );
        let session_id = format!("browser-smoke-{}", uuid::Uuid::now_v7());
        let mut browser = BrowserController::launch(&session_id, false)
            .await
            .expect("launch controlled browser");
        browser
            .evaluate(
                r#"document.body.innerHTML='<input aria-label="Name"><input type="password" value="never-leak-password"><button>Run</button><p>Ready</p><label for="labelled">本次验收文本</label><input id="labelled"><span id="first-name">审批</span><span id="last-name">目标</span><textarea aria-labelledby="first-name last-name"></textarea>'; document.querySelector('button').onclick=()=>document.body.dataset.clicked='yes'; true"#,
                true,
            )
            .await
            .expect("install smoke fixture");
        let snapshot = browser.snapshot().await.expect("snapshot");
        assert!(!snapshot.to_string().contains("never-leak-password"));
        let elements = snapshot["elements"].as_array().expect("elements");
        assert!(elements
            .iter()
            .any(|element| element["name"] == "本次验收文本"));
        assert!(elements
            .iter()
            .any(|element| element["name"] == "审批 目标"));
        let input_ref = elements
            .iter()
            .find(|element| element["name"] == "Name")
            .and_then(|element| element["ref"].as_str())
            .expect("input ref")
            .to_string();
        let button_ref = elements
            .iter()
            .find(|element| element["name"] == "Run")
            .and_then(|element| element["ref"].as_str())
            .expect("button ref")
            .to_string();
        let password_ref = elements
            .iter()
            .find(|element| element["type"] == "password")
            .and_then(|element| element["ref"].as_str())
            .expect("password ref")
            .to_string();
        assert!(browser
            .type_text(&password_ref, "must-never-be-entered", true)
            .await
            .is_err());
        browser
            .type_text(&input_ref, "EchoAgent", true)
            .await
            .expect("type text");
        let approval = browser
            .prepare_interaction(json!({ "kind": "click", "reference": button_ref }))
            .await
            .expect("capture button");
        browser
            .click(&approval, "left", 1)
            .await
            .expect("click button");
        let observed = browser
            .evaluate(
                "({value:document.querySelector('input').value,clicked:document.body.dataset.clicked})",
                true,
            )
            .await
            .expect("observe result");
        assert_eq!(observed["value"], "EchoAgent");
        assert_eq!(observed["clicked"], "yes");
        browser.evaluate(r#"document.body.insertAdjacentHTML('beforeend','<input aria-label="会议日期" type="date" min="2026-01-01" max="2026-12-31"><select aria-label="房间"><option value="a">甲</option><option value="b">乙</option></select>'); true"#, true).await.expect("install typed controls");
        let controls = browser.snapshot().await.expect("typed control snapshot");
        let entries = controls["elements"].as_array().unwrap();
        let date_ref = entries.iter().find(|e| e["name"] == "会议日期").unwrap()["ref"]
            .as_str()
            .unwrap();
        let room_ref = entries.iter().find(|e| e["name"] == "房间").unwrap()["ref"]
            .as_str()
            .unwrap();
        assert_eq!(
            browser
                .type_text(date_ref, "2026-10-23", true)
                .await
                .unwrap()["verified"],
            true
        );
        assert!(browser
            .type_text(date_ref, "2026-02-30", true)
            .await
            .is_err());
        assert_eq!(
            browser.select(room_ref, &["乙".into()]).await.unwrap()["verified"],
            true
        );
        assert!(browser.select(room_ref, &["不存在".into()]).await.is_err());
        let values = browser.evaluate("({date:document.querySelector('input[type=date]').value,room:document.querySelector('select').value})", true).await.unwrap();
        assert_eq!(values["date"], "2026-10-23");
        assert_eq!(values["room"], "b");
        let (image, _) = browser.screenshot().await.expect("screenshot");
        assert!(image.len() > 1_000);
        let second = browser.new_tab(None).await.expect("new tab");
        assert!(browser.tabs().await.expect("tabs").len() >= 2);
        browser.close_tab(&second.id).await.expect("close tab");
        browser.stop().await.expect("stop browser");
        remove_profile(&session_id)
            .await
            .expect("remove test profile");
    }
}
