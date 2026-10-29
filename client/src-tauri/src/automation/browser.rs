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
const EDITABLE_TEXT_JS: &str = include_str!("editable-text.js");
type CdpConnection = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

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
    child: Child,
    _protection: Option<echo_agent_tools::util::process_safety::ProcessLease>,
    port: u16,
    browser_name: String,
    profile_dir: PathBuf,
    target_id: Option<String>,
    allow_private_network: bool,
    network_proxy: BrowserNetworkProxy,
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
        let _ = self.child.start_kill();
    }
}

impl BrowserController {
    pub async fn launch(session_id: &str, allow_private_network: bool) -> Result<Self, String> {
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
            child,
            _protection: protection,
            port,
            browser_name,
            profile_dir,
            target_id: None,
            allow_private_network,
            network_proxy,
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
        self.network_proxy.set_allow_private_network(allowed);
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
        if self
            .child
            .try_wait()
            .map_err(|error| format!("检查浏览器进程失败：{error}"))?
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
        if self
            .child
            .try_wait()
            .map_err(|error| format!("检查浏览器进程失败：{error}"))?
            .is_none()
        {
            self.child
                .kill()
                .await
                .map_err(|error| format!("停止受控浏览器失败：{error}"))?;
        }
        Ok(())
    }

    pub async fn navigate(&mut self, raw_url: &str) -> Result<Value, String> {
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
        let value = self.evaluate("location.href", true).await?;
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "浏览器没有返回当前地址".to_string())
    }

    pub async fn snapshot(&mut self) -> Result<Value, String> {
        // Refs live on DOM nodes and remain stable until the page replaces the
        // node. Hidden/non-interactive content is omitted to keep the model
        // context bounded and focused on actionable UI.
        const SCRIPT: &str = r#"(() => {
          const visible = (el) => {
            const s = getComputedStyle(el), r = el.getBoundingClientRect();
            return s.visibility !== 'hidden' && s.display !== 'none' && r.width > 0 && r.height > 0;
          };
          const selectors = [
            'a[href]','button','input','textarea','select','summary','details',
            '[role="button"]','[role="link"]','[role="checkbox"]','[role="radio"]',
            '[role="tab"]','[role="menuitem"]','[contenteditable="true"]','[tabindex]'
          ].join(',');
          let seq = Number(document.documentElement.dataset.echoRefSeq || 0);
          const nodes = [];
          for (const el of document.querySelectorAll(selectors)) {
            if (!visible(el) || el.closest('[aria-hidden="true"]')) continue;
            if (!el.dataset.echoRef) el.dataset.echoRef = `e${++seq}`;
            const r = el.getBoundingClientRect();
            const inputType = (el.getAttribute('type') || '').toLowerCase();
            const safeValue = inputType === 'password' ? '' : (el.value || '');
            const labelledBy = (el.getAttribute('aria-labelledby') || '').split(/\s+/)
              .filter(Boolean).map(id => document.getElementById(id)?.textContent || '')
              .filter(text => text.trim()).join(' ').trim();
            const associatedLabel = Array.from(el.labels || [])
              .map(label => label.innerText || label.textContent || '').join(' ').trim();
            const label = (labelledBy || el.getAttribute('aria-label') || associatedLabel || el.getAttribute('title') ||
              el.getAttribute('placeholder') || el.innerText || safeValue).trim().slice(0, 300);
            nodes.push({
              ref: el.dataset.echoRef,
              tag: el.tagName.toLowerCase(),
              role: el.getAttribute('role') || '',
              name: label,
              type: inputType,
              disabled: !!el.disabled || el.getAttribute('aria-disabled') === 'true',
              checked: typeof el.checked === 'boolean' ? el.checked : undefined,
              href: el.href || undefined,
              value: inputType === 'password' ? undefined : String(safeValue).slice(0, 300),
              bounds: { x: Math.round(r.x), y: Math.round(r.y), width: Math.round(r.width), height: Math.round(r.height) }
            });
            if (nodes.length >= 500) break;
          }
          document.documentElement.dataset.echoRefSeq = String(seq);
          return {
            url: location.href,
            title: document.title,
            viewport: { width: innerWidth, height: innerHeight, scrollX, scrollY },
            elements: nodes,
            text: (document.body?.innerText || '').replace(/\s+/g, ' ').trim().slice(0, 12000)
          };
        })()"#;
        self.evaluate(SCRIPT, true).await
    }

    pub async fn screenshot(&mut self) -> Result<(String, Value), String> {
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

    pub async fn click(
        &mut self,
        element_ref: Option<&str>,
        x: Option<f64>,
        y: Option<f64>,
        button: &str,
        click_count: u32,
    ) -> Result<Value, String> {
        let (x, y) = match element_ref {
            Some(reference) => {
                let reference = serde_json::to_string(reference)
                    .map_err(|error| format!("编码元素引用失败：{error}"))?;
                let expression = format!(
                    "(() => {{ const e=document.querySelector('[data-echo-ref='+CSS.escape({reference})+']'); if(!e) return null; const r=e.getBoundingClientRect(); e.scrollIntoView({{block:'center',inline:'center'}}); const q=e.getBoundingClientRect(); return {{x:q.left+q.width/2,y:q.top+q.height/2,tag:e.tagName.toLowerCase(),name:(e.innerText||e.getAttribute('aria-label')||'').trim().slice(0,200),type:e.type||''}}; }})()"
                );
                let location = self.evaluate(&expression, true).await?;
                let x = location
                    .get("x")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?;
                let y = location
                    .get("y")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?;
                (x, y)
            }
            None => (
                x.ok_or_else(|| "click 需要 elementRef 或 x/y 坐标".to_string())?,
                y.ok_or_else(|| "click 需要 elementRef 或 x/y 坐标".to_string())?,
            ),
        };
        let button = match button {
            "left" | "right" | "middle" => button,
            _ => return Err("button 只能是 left、right 或 middle".into()),
        };
        self.call_page(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none" }),
        )
        .await?;
        self.call_page(
            "Input.dispatchMouseEvent",
            json!({ "type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": click_count.clamp(1, 3) }),
        )
        .await?;
        let release = json!({ "type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": click_count.clamp(1, 3) });
        if let Err(error) = self
            .call_page("Input.dispatchMouseEvent", release.clone())
            .await
        {
            let _ = self.call_page("Input.dispatchMouseEvent", release).await;
            return Err(error);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        Ok(json!({ "clicked": true, "x": x, "y": y, "button": button }))
    }

    pub async fn hover(&mut self, element_ref: &str) -> Result<Value, String> {
        let reference = serde_json::to_string(element_ref)
            .map_err(|error| format!("编码元素引用失败：{error}"))?;
        let expression = format!(
            "(() => {{ const e=document.querySelector('[data-echo-ref='+CSS.escape({reference})+']'); if(!e) return null; e.scrollIntoView({{block:'center',inline:'center'}}); const r=e.getBoundingClientRect(); return {{x:r.left+r.width/2,y:r.top+r.height/2}}; }})()"
        );
        let location = self.evaluate(&expression, true).await?;
        let x = location
            .get("x")
            .and_then(Value::as_f64)
            .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?;
        let y = location
            .get("y")
            .and_then(Value::as_f64)
            .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?;
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
            "(() => {{ const editableText={EDITABLE_TEXT_JS}; const e=document.querySelector('[data-echo-ref='+CSS.escape({reference})+']'); if(!e) return null; if(e.matches(':disabled')||e.readOnly||e.getAttribute('aria-disabled')==='true') return {{disabled:true}}; const type=(e.type||'').toLowerCase(); if(type==='password') return {{type}}; if(!(e instanceof HTMLTextAreaElement)&&!(e instanceof HTMLInputElement)&&!e.isContentEditable) return {{unsupported:true}}; if(['checkbox','radio','file','button','submit','reset','hidden','color','range'].includes(type)) return {{unsupported:true,type}}; e.scrollIntoView({{block:'center',inline:'center'}}); e.focus(); {} return {{disabled:false,type,tag:e.tagName.toLowerCase(),editable:e.isContentEditable,before:e.isContentEditable?editableText(e):e.value}}; }})()",
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
                "(() => {{ const e=document.querySelector('[data-echo-ref='+CSS.escape({reference})+']'); if(!(e instanceof HTMLInputElement)||e.matches(':disabled')||e.readOnly||e.type!=={}) throw new Error('输入目标已变化'); const probe=e.cloneNode(false); probe.value={value}; if(probe.value!=={value}||!probe.checkValidity()) throw new Error('输入值不符合控件的日期、格式、范围或步长要求，原字段值保持'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,{value}); e.dispatchEvent(new Event('input',{{bubbles:true}})); e.dispatchEvent(new Event('change',{{bubbles:true}})); return true; }})()",
                serde_json::to_string(input_type).map_err(|e| e.to_string())?
            ), true).await?;
        } else {
            if text.is_empty() && replace {
                self.key("Backspace", &[]).await?;
            } else {
                self.call_page("Input.insertText", json!({ "text": text }))
                    .await?;
            }
        }
        let readback = self.evaluate(&format!(
            "(() => {{ const editableText={EDITABLE_TEXT_JS}; const e=document.querySelector('[data-echo-ref='+CSS.escape({reference})+']'); if(!e||e.type==='password') return null; return {{value:e.isContentEditable?editableText(e):e.value,valid:typeof e.checkValidity==='function'?e.checkValidity():true}}; }})()"
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
            "(() => {{ const e=document.querySelector('[data-echo-ref='+CSS.escape({reference})+']'); if(!(e instanceof HTMLSelectElement)) return null; if(e.matches(':disabled')||e.getAttribute('aria-disabled')==='true') return {{error:'目标下拉框已禁用'}}; const values={values_json}; if(!e.multiple&&values.length!==1) return {{error:'单选下拉框只能选择一个值'}}; const options=Array.from(e.options); const matched=values.map(v=>options.filter(o=>!o.disabled&&!o.parentElement?.disabled&&(o.value===v||o.text===v))); if(matched.some(m=>m.length!==1)) return {{error:'选项不存在、不可用或名称不唯一，请重新获取页面快照'}}; const wanted=new Set(matched.map(m=>m[0])); for(const o of options) o.selected=wanted.has(o); e.dispatchEvent(new Event('input',{{bubbles:true}})); e.dispatchEvent(new Event('change',{{bubbles:true}})); const selected=Array.from(e.selectedOptions); return {{selected:selected.map(o=>({{value:o.value,text:o.text}})),verified:e.isConnected&&selected.length===wanted.size&&selected.every(o=>wanted.has(o)),valid:e.checkValidity()}}; }})()"
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
        let document = self
            .call_page("DOM.getDocument", json!({ "depth": 0, "pierce": true }))
            .await?;
        let root_id = document
            .pointer("/root/nodeId")
            .and_then(Value::as_u64)
            .ok_or_else(|| "浏览器未返回 DOM 根节点".to_string())?;
        if !element_ref
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        {
            return Err("页面元素引用无效".into());
        }
        let selector = format!(r#"[data-echo-ref="{element_ref}"]"#);
        let queried = self
            .call_page(
                "DOM.querySelector",
                json!({ "nodeId": root_id, "selector": selector }),
            )
            .await?;
        let node_id = queried
            .get("nodeId")
            .and_then(Value::as_u64)
            .filter(|id| *id != 0)
            .ok_or_else(|| "页面元素已失效，请重新获取页面快照".to_string())?;
        self.call_page(
            "DOM.setFileInputFiles",
            json!({ "nodeId": node_id, "files": canonical_paths }),
        )
        .await?;
        Ok(json!({ "uploaded": true, "fileCount": canonical_paths.len() }))
    }

    pub fn downloads(&self) -> Result<Vec<BrowserDownload>, String> {
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

    pub async fn key(&mut self, key: &str, modifiers: &[String]) -> Result<Value, String> {
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
        let mut down = base.clone();
        down["type"] = Value::String("keyDown".into());
        self.call_page("Input.dispatchKeyEvent", down).await?;
        let mut up = base;
        up["type"] = Value::String("keyUp".into());
        if let Err(error) = self.call_page("Input.dispatchKeyEvent", up.clone()).await {
            let _ = self.call_page("Input.dispatchKeyEvent", up).await;
            return Err(error);
        }
        Ok(json!({ "pressed": key, "modifiers": modifiers }))
    }

    pub async fn scroll(&mut self, delta_x: f64, delta_y: f64) -> Result<Value, String> {
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

    pub async fn drag(
        &mut self,
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
    ) -> Result<Value, String> {
        self.call_page(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": from_x, "y": from_y }),
        )
        .await?;
        self.call_page(
            "Input.dispatchMouseEvent",
            json!({ "type": "mousePressed", "x": from_x, "y": from_y, "button": "left", "clickCount": 1 }),
        )
        .await?;
        let movement = async {
            for step in 1..=8 {
                let fraction = f64::from(step) / 8.0;
                let x = from_x + (to_x - from_x) * fraction;
                let y = from_y + (to_y - from_y) * fraction;
                self.call_page(
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseMoved", "x": x, "y": y, "button": "left", "buttons": 1 }),
                )
                .await?;
            }
            Ok::<(), String>(())
        }
        .await;
        let release_event = json!({ "type": "mouseReleased", "x": to_x, "y": to_y, "button": "left", "clickCount": 1 });
        let release = match self
            .call_page("Input.dispatchMouseEvent", release_event.clone())
            .await
        {
            Ok(_) => Ok(()),
            Err(error) => {
                let _ = self
                    .call_page("Input.dispatchMouseEvent", release_event)
                    .await;
                Err(error)
            }
        };
        movement.and(release)?;
        Ok(json!({ "dragged": true, "from": [from_x, from_y], "to": [to_x, to_y] }))
    }

    pub async fn tabs(&mut self) -> Result<Vec<BrowserTarget>, String> {
        self.targets().await
    }

    pub async fn new_tab(&mut self, raw_url: Option<&str>) -> Result<BrowserTarget, String> {
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
        let endpoint = format!("http://127.0.0.1:{}/json/close/{target_id}", self.port);
        control_client()?
            .get(endpoint)
            .send()
            .await
            .map_err(|error| format!("关闭浏览器标签页失败：{error}"))?
            .error_for_status()
            .map_err(|error| format!("关闭浏览器标签页失败：{error}"))?;
        if self.target_id.as_deref() == Some(target_id) {
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

    fn download_dir(&self) -> PathBuf {
        self.profile_dir.join("Downloads")
    }

    async fn evaluate(&mut self, expression: &str, return_by_value: bool) -> Result<Value, String> {
        let result = self
            .call_page(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": return_by_value,
                    "awaitPromise": true,
                    "userGesture": true,
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(format!(
                "页面脚本执行失败：{}",
                compact_json(exception, 2_000)
            ));
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
        let future = async {
            // Target discovery is part of the command deadline. Otherwise a
            // wedged local DevTools HTTP endpoint could hold the browser mutex
            // forever and make pause/stop/status appear frozen.
            let target = self.ensure_page().await?;
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
            let (mut websocket, _) =
                tokio_tungstenite::connect_async(&target.websocket_debugger_url)
                    .await
                    .map_err(|error| format!("连接受控浏览器失败：{error}"))?;
            cdp_request(&mut websocket, id, method, params).await
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
                browser
                    .click(Some(&reference), None, None, "left", 1)
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
        browser
            .click(Some(&button_ref), None, None, "left", 1)
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
