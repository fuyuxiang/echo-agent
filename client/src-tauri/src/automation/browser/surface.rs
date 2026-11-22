use super::{
    cdp_request, isolated_targets, parse_navigation_url, BrowserController, BrowserNetworkProxy,
    BrowserTarget, CdpConnection,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Document {
    target_id: String,
    loader_id: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum PanelAction {
    Open {
        url: String,
        #[serde(default)]
        allow_development: bool,
    },
    Navigate {
        url: String,
        #[serde(default)]
        allow_development: bool,
    },
    RevokeDevelopment,
    New,
    Select {
        id: String,
    },
    Close {
        id: String,
    },
    Back,
    Forward,
    Reload,
    Input {
        document: Document,
        frame_id: String,
        input: PageInput,
    },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum PageInput {
    Pointer {
        x: f64,
        y: f64,
        destination: Option<[f64; 2]>,
        button: PointerButton,
        count: u8,
        modifiers: u8,
    },
    Move {
        x: f64,
        y: f64,
    },
    Wheel {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    Key {
        key: String,
        code: String,
        virtual_key: u32,
        modifiers: u8,
    },
    Text {
        text: String,
    },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PointerButton {
    Left,
    Middle,
    Right,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PanelView {
    tabs: Vec<PanelTab>,
    active_id: Option<String>,
    external: bool,
    allow_private_network: bool,
    development_endpoints: Vec<String>,
    page: Option<PanelPage>,
}

impl PanelView {
    pub(crate) fn empty(external: bool, allow_private_network: bool) -> Self {
        Self {
            tabs: vec![],
            active_id: None,
            external,
            allow_private_network,
            development_endpoints: vec![],
            page: None,
        }
    }
}

#[derive(Serialize)]
struct PanelTab {
    id: String,
    title: String,
    url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PanelPage {
    document: Document,
    can_go_back: bool,
    can_go_forward: bool,
    frame_id: Option<String>,
    image: Option<String>,
    width: f64,
    height: f64,
    error: bool,
}

impl PanelAction {
    pub(crate) fn opens_browser(&self) -> bool {
        matches!(self, Self::Open { .. } | Self::Navigate { .. } | Self::New)
    }
}

#[derive(Clone)]
struct PanelFrame {
    id: String,
    document: Document,
    viewport: Value,
    image: Arc<str>,
}

#[derive(Clone, Default)]
pub(super) struct PanelFrames(Arc<Mutex<VecDeque<PanelFrame>>>);

impl PanelFrames {
    fn remember(&self, document: &Document, viewport: Value, image: &str) -> String {
        let mut frames = self.0.lock().unwrap();
        if let Some(frame) = frames.back() {
            if frame.document == *document
                && frame.viewport == viewport
                && frame.image.as_ref() == image
            {
                return frame.id.clone();
            }
        }
        let id = uuid::Uuid::now_v7().to_string();
        frames.push_back(PanelFrame {
            id: id.clone(),
            document: document.clone(),
            viewport,
            image: Arc::from(image),
        });
        if frames.len() > 6 {
            frames.pop_front();
        }
        id
    }

    fn get(&self, id: &str) -> Option<PanelFrame> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|frame| frame.id == id)
            .cloned()
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

pub(crate) struct PanelSource {
    port: u16,
    extension: Option<Arc<super::super::browser_bridge::Connection>>,
    target_id: Option<String>,
    frames: PanelFrames,
    allow_private_network: bool,
    development_endpoints: Vec<String>,
}

struct CaptureConnection {
    socket: CdpConnection,
    next_id: u64,
}

impl CaptureConnection {
    async fn connect(target: &BrowserTarget) -> Result<Self, String> {
        let (socket, _) = tokio_tungstenite::connect_async(&target.websocket_debugger_url)
            .await
            .map_err(|error| format!("连接网页画面失败：{error}"))?;
        Ok(Self { socket, next_id: 1 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        cdp_request(&mut self.socket, id, method, params).await
    }
}

impl PanelSource {
    pub(crate) fn connected(
        extension: Arc<super::super::browser_bridge::Connection>,
        allow_private_network: bool,
    ) -> Self {
        Self {
            port: 0,
            extension: Some(extension),
            target_id: None,
            frames: PanelFrames::default(),
            allow_private_network,
            development_endpoints: vec![],
        }
    }

    pub(crate) async fn view(self, known_frame: Option<&str>) -> Result<PanelView, String> {
        let external = self.extension.is_some();
        let targets = if let Some(extension) = &self.extension {
            serde_json::from_value(extension.call("Echo.tabs", json!({}), None).await?)
                .map_err(|error| format!("读取已连接浏览器的标签失败：{error}"))?
        } else {
            isolated_targets(self.port).await?
        };
        let tabs: Vec<BrowserTarget> = targets.into_iter().filter(is_panel_tab).collect();
        let target = tabs
            .iter()
            .find(|tab| Some(&tab.id) == self.target_id.as_ref())
            .or_else(|| tabs.first());
        let active_id = target.map(|tab| tab.id.clone());
        let page = match target {
            Some(tab) if !external => self.page(tab, known_frame).await?,
            _ => None,
        };
        Ok(PanelView {
            tabs: tabs
                .into_iter()
                .map(|tab| PanelTab {
                    id: tab.id,
                    title: tab.title,
                    url: tab.url,
                })
                .collect(),
            active_id,
            external,
            allow_private_network: self.allow_private_network,
            development_endpoints: self.development_endpoints,
            page,
        })
    }

    async fn page(
        &self,
        tab: &BrowserTarget,
        known_frame: Option<&str>,
    ) -> Result<Option<PanelPage>, String> {
        let mut capture = CaptureConnection::connect(tab).await?;
        let (document, error) = read_document(
            &tab.id,
            &capture.call("Page.getFrameTree", json!({})).await?,
        )?;
        let history = capture.call("Page.getNavigationHistory", json!({})).await?;
        let index = history["currentIndex"].as_u64().unwrap_or_default() as usize;
        let entries = history["entries"].as_array().map_or(0, Vec::len);
        let mut page = PanelPage {
            document,
            can_go_back: index > 0,
            can_go_forward: index + 1 < entries,
            frame_id: None,
            image: None,
            width: 0.0,
            height: 0.0,
            error,
        };
        if page.document.url == "about:blank" || error {
            return Ok(Some(page));
        }

        let layout = capture.call("Page.getLayoutMetrics", json!({})).await?;
        let viewport = &layout["cssVisualViewport"];
        page.width = viewport["clientWidth"].as_f64().ok_or("无法读取网页宽度")?;
        page.height = viewport["clientHeight"]
            .as_f64()
            .ok_or("无法读取网页高度")?;
        let screenshot = capture
            .call("Page.captureScreenshot", screenshot_options())
            .await?;
        let (current, failed) = read_document(
            &tab.id,
            &capture.call("Page.getFrameTree", json!({})).await?,
        )?;
        let current_layout = capture.call("Page.getLayoutMetrics", json!({})).await?;
        if failed || page.document != current || current_layout["cssVisualViewport"] != *viewport {
            return Ok(None);
        }
        let image = screenshot["data"].as_str().ok_or("浏览器未返回画面")?;
        let id = self.frames.remember(&current, viewport.clone(), image);
        if known_frame != Some(id.as_str()) {
            page.image = Some(image.to_owned());
        }
        page.frame_id = Some(id);
        Ok(Some(page))
    }
}

fn is_panel_tab(tab: &BrowserTarget) -> bool {
    tab.target_type == "page" && !tab.websocket_debugger_url.is_empty()
}

fn read_document(id: &str, tree: &Value) -> Result<(Document, bool), String> {
    let main = &tree["frameTree"]["frame"];
    let url = main["url"].as_str().ok_or("无法读取当前网页地址")?;
    let fragment = main["urlFragment"].as_str().unwrap_or_default();
    let unreachable = main["unreachableUrl"].as_str();
    let failed = unreachable.is_some() || parse_navigation_url(url).is_err();
    Ok((
        Document {
            target_id: id.to_owned(),
            loader_id: main["loaderId"]
                .as_str()
                .ok_or("无法读取当前网页标识")?
                .to_owned(),
            url: unreachable
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{url}{fragment}")),
        },
        failed,
    ))
}

fn screenshot_options() -> Value {
    json!({ "format": "jpeg", "quality": 76, "fromSurface": true, "captureBeyondViewport": false })
}

impl BrowserController {
    pub(crate) fn has_current_tab(&self) -> bool {
        self.target_id.is_some()
    }

    pub(crate) fn panel_source(&self) -> PanelSource {
        PanelSource {
            port: self.port,
            extension: self.extension.clone(),
            target_id: self.target_id.clone(),
            frames: self.panel_frames.clone(),
            allow_private_network: self.allow_private_network,
            development_endpoints: self
                .network_proxy
                .as_ref()
                .map(BrowserNetworkProxy::development_endpoints)
                .unwrap_or_default(),
        }
    }

    pub(crate) async fn resize_panel(&mut self, size: Option<(u32, u32)>) -> Result<(), String> {
        let Some(((width, height), target)) = size
            .zip(self.target_id.clone())
            .filter(|_| self.extension.is_none())
        else {
            return Ok(());
        };
        let size = (target, width.clamp(360, 1600), height.clamp(240, 1400));
        if self.panel_size.as_ref() != Some(&size) {
            self.panel_frames.clear();
            self.call_page_at(
                Some(&size.0),
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": size.1, "height": size.2, "deviceScaleFactor": 1, "mobile": false,
                }),
            )
            .await?;
            self.panel_size = Some(size);
        }
        Ok(())
    }

    async fn panel_document(&mut self, id: &str) -> Result<(Document, bool), String> {
        read_document(
            id,
            &self
                .call_page_at(Some(id), "Page.getFrameTree", json!({}))
                .await?,
        )
    }

    async fn check_panel_pointer(
        &mut self,
        tab: &BrowserTarget,
        document: &Document,
        frame_id: &str,
        points: &[[f64; 2]],
    ) -> Result<(), String> {
        const STALE: &str = "网页画面已变化，请在更新后的画面上重试";
        let frame = self
            .panel_frames
            .get(frame_id)
            .filter(|frame| &frame.document == document)
            .ok_or(STALE)?;
        let mut capture = CaptureConnection::connect(tab).await?;
        let now = capture.call("Page.getLayoutMetrics", json!({})).await?;
        if now["cssVisualViewport"] != frame.viewport {
            return Err(STALE.into());
        }
        let screenshot = capture
            .call("Page.captureScreenshot", screenshot_options())
            .await?;
        let (current, error) = read_document(
            &tab.id,
            &capture.call("Page.getFrameTree", json!({})).await?,
        )?;
        let now = capture.call("Page.getLayoutMetrics", json!({})).await?;
        if error || current != *document || now["cssVisualViewport"] != frame.viewport {
            return Err(STALE.into());
        }
        let latest = screenshot["data"].as_str().ok_or(STALE)?;
        if !same_pointer_pixels(&frame, latest, points)? {
            return Err(STALE.into());
        }
        Ok(())
    }

    pub(crate) async fn panel_action(
        &mut self,
        expected: Option<&str>,
        action: PanelAction,
    ) -> Result<(), String> {
        if matches!(action, PanelAction::RevokeDevelopment) {
            if let Some(proxy) = &self.network_proxy {
                proxy.revoke_development();
                self.panel_frames.clear();
                let _ = self.enforce_current_url_policy().await;
            }
            return Ok(());
        }
        let active = self.ensure_page().await?;
        if expected.is_some_and(|id| id != active.id) {
            return Err("当前标签页已变化，请重试".into());
        }
        if self.extension.is_some() && !matches!(action, PanelAction::Select { .. }) {
            return Err("请在已连接的浏览器中操作此标签页".into());
        }
        let open_tab = matches!(action, PanelAction::Open { .. });
        let allow_development = matches!(
            action,
            PanelAction::Navigate {
                allow_development: true,
                ..
            } | PanelAction::Open {
                allow_development: true,
                ..
            }
        );
        match action {
            PanelAction::Open { url, .. } | PanelAction::Navigate { url, .. } => {
                let parsed = parse_navigation_url(&url)?;
                if allow_development {
                    self.network_proxy
                        .as_ref()
                        .ok_or("当前浏览器不支持本地端口授权")?
                        .allow_development_url(&parsed)?;
                }
                self.panel_frames.clear();
                if open_tab && active.url != "about:blank" {
                    self.new_tab(None).await?;
                }
                self.navigate(parsed.as_str()).await?;
            }
            PanelAction::New => {
                self.new_tab(None).await?;
            }
            PanelAction::Select { id } => {
                self.select_tab(&id).await?;
            }
            PanelAction::Close { id } => {
                if self
                    .targets()
                    .await?
                    .iter()
                    .any(|tab| tab.id == id && is_panel_tab(tab))
                {
                    self.close_tab(&id).await?;
                }
            }
            PanelAction::Back | PanelAction::Forward => {
                let history = self
                    .call_page("Page.getNavigationHistory", json!({}))
                    .await?;
                let offset = if matches!(action, PanelAction::Back) {
                    -1
                } else {
                    1
                };
                if let Some(index) = history["currentIndex"]
                    .as_i64()
                    .and_then(|index| usize::try_from(index + offset).ok())
                {
                    if let Some(entry_id) = history["entries"]
                        .get(index)
                        .and_then(|entry| entry["id"].as_i64())
                    {
                        self.call_page(
                            "Page.navigateToHistoryEntry",
                            json!({ "entryId": entry_id }),
                        )
                        .await?;
                    }
                }
            }
            PanelAction::Reload => {
                self.call_page("Page.reload", json!({})).await?;
            }
            PanelAction::Input {
                document,
                frame_id,
                input,
            } => {
                let (current, failed) = self.panel_document(&active.id).await?;
                if failed
                    || current.target_id != document.target_id
                    || current.loader_id != document.loader_id
                    || current.url != document.url
                {
                    return Err("网页刚刚跳转，请在新页面上重试".into());
                }
                if let PageInput::Pointer {
                    x, y, destination, ..
                } = &input
                {
                    let mut points = vec![[*x, *y]];
                    points.extend(*destination);
                    for [x, y] in &points {
                        validate_point(*x, *y)?;
                    }
                    self.check_panel_pointer(&active, &document, &frame_id, &points)
                        .await?;
                }
                self.panel_input(&active.id, input).await?;
            }
            PanelAction::RevokeDevelopment => unreachable!(),
        }
        Ok(())
    }

    async fn panel_input(&mut self, id: &str, input: PageInput) -> Result<(), String> {
        match input {
            PageInput::Pointer {
                x,
                y,
                destination,
                button,
                count,
                modifiers,
            } => {
                let count = if destination.is_some() {
                    0
                } else {
                    count.clamp(1, 3)
                };
                let modifiers = modifiers & 15;
                let buttons = match button {
                    PointerButton::Left => 1,
                    PointerButton::Right => 2,
                    PointerButton::Middle => 4,
                };
                let event = |kind: &str, x, y, buttons, count| {
                    json!({
                        "type": kind, "x": x, "y": y, "button": button,
                        "buttons": buttons, "clickCount": count, "modifiers": modifiers,
                    })
                };
                self.call_page_at(
                    Some(id),
                    "Input.dispatchMouseEvent",
                    event("mousePressed", x, y, buttons, count),
                )
                .await?;
                let movement = if let Some([end_x, end_y]) = destination {
                    let mut result = Ok(json!({}));
                    for step in 1..=4 {
                        let fraction = f64::from(step) / 4.0;
                        result = self
                            .call_page_at(
                                Some(id),
                                "Input.dispatchMouseEvent",
                                event(
                                    "mouseMoved",
                                    x + (end_x - x) * fraction,
                                    y + (end_y - y) * fraction,
                                    buttons,
                                    0,
                                ),
                            )
                            .await;
                        if result.is_err() {
                            break;
                        }
                    }
                    result
                } else {
                    Ok(json!({}))
                };
                let [end_x, end_y] = destination.unwrap_or([x, y]);
                let released = self
                    .call_page_at(
                        Some(id),
                        "Input.dispatchMouseEvent",
                        event("mouseReleased", end_x, end_y, 0, count),
                    )
                    .await;
                movement?;
                released?;
            }
            PageInput::Move { x, y } => {
                validate_point(x, y)?;
                self.call_page_at(
                    Some(id),
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": "mouseMoved", "x": x, "y": y,
                    }),
                )
                .await?;
            }
            PageInput::Wheel { x, y, dx, dy } => {
                validate_point(x, y)?;
                self.call_page_at(Some(id), "Input.dispatchMouseEvent", json!({
                    "type": "mouseWheel", "x": x, "y": y, "deltaX": dx.clamp(-2400.0, 2400.0), "deltaY": dy.clamp(-2400.0, 2400.0),
                })).await?;
            }
            PageInput::Key {
                key,
                code,
                virtual_key,
                modifiers,
            } => {
                if key.len() > 64 || code.len() > 64 {
                    return Err("键盘输入过长".into());
                }
                let printable = key.chars().count() == 1 && modifiers & 7 == 0;
                let mut event = json!({
                    "type": if printable { "keyDown" } else { "rawKeyDown" },
                    "key": key, "code": code, "windowsVirtualKeyCode": virtual_key, "modifiers": modifiers & 15,
                });
                if printable {
                    event["text"] = json!(key);
                }
                self.call_page_at(Some(id), "Input.dispatchKeyEvent", event.clone())
                    .await?;
                event["type"] = json!("keyUp");
                event.as_object_mut().unwrap().remove("text");
                self.call_page_at(Some(id), "Input.dispatchKeyEvent", event)
                    .await?;
            }
            PageInput::Text { text } => {
                if text.len() > 65_536 {
                    return Err("粘贴内容超过 64 KB".into());
                }
                self.call_page_at(Some(id), "Input.insertText", json!({ "text": text }))
                    .await?;
            }
        }
        Ok(())
    }
}

fn validate_point(x: f64, y: f64) -> Result<(), String> {
    if [x, y]
        .into_iter()
        .all(|point| (0.0..8192.0).contains(&point))
    {
        Ok(())
    } else {
        Err("输入坐标超出网页范围".into())
    }
}

fn same_pointer_pixels(
    frame: &PanelFrame,
    latest: &str,
    points: &[[f64; 2]],
) -> Result<bool, String> {
    let decode = |data: &str| {
        let bytes = STANDARD.decode(data).map_err(|_| "网页画面数据无效")?;
        image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
            .map(|image| image.into_rgb8())
            .map_err(|_| "无法校验网页画面")
    };
    let width = frame.viewport["clientWidth"]
        .as_f64()
        .ok_or("网页画面尺寸无效")?;
    let height = frame.viewport["clientHeight"]
        .as_f64()
        .ok_or("网页画面尺寸无效")?;
    if width <= 0.0 || height <= 0.0 || points.iter().any(|[x, y]| *x >= width || *y >= height) {
        return Ok(false);
    }
    if frame.image.as_ref() == latest {
        return Ok(true);
    }
    let before = decode(&frame.image)?;
    let after = decode(latest)?;
    if before.dimensions() != after.dimensions() {
        return Ok(false);
    }
    let (pixels_x, pixels_y) = before.dimensions();
    Ok(points.iter().all(|[x, y]| {
        let x = (*x * f64::from(pixels_x) / width) as u32;
        let y = (*y * f64::from(pixels_y) / height) as u32;
        (y.saturating_sub(28)..(y + 29).min(pixels_y)).all(|row| {
            (x.saturating_sub(28)..(x + 29).min(pixels_x))
                .all(|col| before.get_pixel(col, row) == after.get_pixel(col, row))
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_validation_ignores_unrelated_changes_and_rejects_replaced_pixels() {
        use image::{codecs::jpeg::JpegEncoder, Rgb, RgbImage};

        let encode = |image: &RgbImage| {
            let mut bytes = Vec::new();
            JpegEncoder::new_with_quality(&mut bytes, 76)
                .encode_image(image)
                .unwrap();
            STANDARD.encode(bytes)
        };
        let mut image = RgbImage::from_pixel(160, 160, Rgb([240, 240, 240]));
        let document = Document {
            target_id: "tab".into(),
            loader_id: "loader".into(),
            url: "https://fixture.invalid/".into(),
        };
        let viewport = json!({"clientWidth":160,"clientHeight":160});
        let frames = PanelFrames::default();
        let frame_id = frames.remember(&document, viewport.clone(), &encode(&image));
        assert_eq!(
            frame_id,
            frames.remember(&document, viewport, &encode(&image))
        );
        let frame = frames.get(&frame_id).unwrap();
        image.put_pixel(150, 150, Rgb([0, 0, 0]));
        assert!(same_pointer_pixels(&frame, &encode(&image), &[[45.0, 45.0]]).unwrap());
        image.put_pixel(45, 45, Rgb([0, 0, 0]));
        assert!(!same_pointer_pixels(&frame, &encode(&image), &[[45.0, 45.0]]).unwrap());
        assert!(!same_pointer_pixels(&frame, &encode(&image), &[[160.0, 40.0]]).unwrap());
    }

    #[tokio::test]
    #[ignore = "requires isolated ECHO_AGENT_HOME and Chromium"]
    async fn controlled_browser_panel_keeps_the_page_and_rejects_a_stale_click() {
        use std::net::Ipv4Addr;
        assert!(crate::paths::redirected_data_home().is_some());
        let session = format!("browser-panel-{}", uuid::Uuid::now_v7());
        let fixture = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = fixture.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let router = axum::Router::new().route("/", axum::routing::get(|| async {
                axum::response::Html(r#"<!doctype html><title>Panel test</title><style>
                  body{margin:0}button{position:absolute;left:30px;top:30px;width:120px;height:44px;background:white}
                  input{position:absolute;left:200px;top:30px}
                  </style><button onclick="document.body.dataset.clicked='yes'">Confirm</button>
                  <input aria-label="Draft">"#)
            }));
            axum::serve(fixture, router).await
        });
        let url = format!("http://localhost:{}/", address.port());
        let mut browser = BrowserController::launch_for_panel(&session, false)
            .await
            .unwrap();
        assert!(browser
            .panel_action(
                None,
                PanelAction::Open {
                    url: url.clone(),
                    allow_development: false
                }
            )
            .await
            .is_err());
        browser
            .panel_action(
                None,
                PanelAction::Navigate {
                    url,
                    allow_development: true,
                },
            )
            .await
            .unwrap();
        browser.resize_panel(Some((440, 320))).await.unwrap();
        let initial = browser.panel_source().view(None).await.unwrap();
        let page = initial.page.unwrap();
        assert_eq!(page.width, 440.0);
        assert!(page.image.is_some());
        let frame_id = page.frame_id.unwrap();
        let unchanged = browser
            .panel_source()
            .view(Some(&frame_id))
            .await
            .unwrap()
            .page
            .unwrap();
        assert!(unchanged.image.is_none());
        let action = |frame_id, document| PanelAction::Input {
            frame_id,
            document,
            input: PageInput::Pointer {
                x: 60.0,
                y: 50.0,
                destination: None,
                button: PointerButton::Left,
                count: 1,
                modifiers: 0,
            },
        };
        browser
            .evaluate(
                "document.querySelector('button').style.background='red'; true",
                true,
            )
            .await
            .unwrap();
        assert!(browser
            .panel_action(
                initial.active_id.as_deref(),
                action(frame_id, page.document)
            )
            .await
            .is_err());
        let refreshed = browser
            .panel_source()
            .view(None)
            .await
            .unwrap()
            .page
            .unwrap();
        browser
            .panel_action(
                initial.active_id.as_deref(),
                action(refreshed.frame_id.unwrap(), refreshed.document),
            )
            .await
            .unwrap();
        assert_eq!(
            browser
                .evaluate("document.body.dataset.clicked", true)
                .await
                .unwrap(),
            "yes"
        );
        let form = browser
            .panel_source()
            .view(None)
            .await
            .unwrap()
            .page
            .unwrap();
        let form_frame = form.frame_id.unwrap();
        let form_document = form.document;
        browser
            .panel_action(
                initial.active_id.as_deref(),
                PanelAction::Input {
                    document: form_document.clone(),
                    frame_id: form_frame.clone(),
                    input: PageInput::Pointer {
                        x: 230.0,
                        y: 40.0,
                        destination: None,
                        button: PointerButton::Left,
                        count: 1,
                        modifiers: 0,
                    },
                },
            )
            .await
            .unwrap();
        browser
            .panel_action(
                initial.active_id.as_deref(),
                PanelAction::Input {
                    document: form_document,
                    frame_id: form_frame,
                    input: PageInput::Text {
                        text: "未提交的草稿".into(),
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            browser
                .evaluate("document.querySelector('input').value", true)
                .await
                .unwrap(),
            "未提交的草稿"
        );
        browser
            .panel_action(None, PanelAction::RevokeDevelopment)
            .await
            .unwrap();
        assert!(browser
            .panel_source()
            .view(None)
            .await
            .unwrap()
            .development_endpoints
            .is_empty());
        browser.stop().await.unwrap();
        super::super::remove_profile(&session).await.unwrap();
        server.abort();
    }
}
