//! Interactive webpages live in a separate native WebView with no Tauri IPC.
//! Only the chat shell can create views, position them, or grant a local port.

use serde::{Deserialize, Serialize};
use tauri::WebviewWindow;

#[cfg(any(target_os = "macos", windows))]
mod artifact_editor;

#[cfg(any(target_os = "macos", windows))]
pub(super) use desktop::SharedBrowser;

#[cfg(any(target_os = "macos", windows))]
pub(super) fn network_available() -> bool {
    #[cfg(windows)]
    return true;
    #[cfg(target_os = "macos")]
    unsafe {
        objc2::msg_send![
            objc2::class!(WKWebsiteDataStore),
            instancesRespondToSelector: objc2::sel!(setProxyConfigurations:)
        ]
    }
}

pub(super) fn artifact_available() -> bool {
    cfg!(any(target_os = "macos", windows))
}

pub(super) fn web_available() -> bool {
    #[cfg(any(target_os = "macos", windows))]
    {
        network_available()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

#[cfg(any(target_os = "macos", windows))]
pub(super) async fn set_private_network(
    app: &tauri::AppHandle,
    session: &str,
    allowed: bool,
    account: Option<u64>,
) -> Result<(), String> {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        let browser = desktop::browser(window, session).await;
        if allowed && !browser.belongs_to_generation(account) {
            return Err("账号已切换，请重新确认访问范围".into());
        }
        browser.set_private_network(allowed).await?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows))]
pub(super) async fn release_input(app: &tauri::AppHandle, session: &str) -> Result<(), String> {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        desktop::browser(window, session)
            .await
            .set_agent_controlled(false)
            .await?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows))]
pub(super) async fn has_tab(app: &tauri::AppHandle, session: &str) -> bool {
    use tauri::Manager;
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };
    desktop::has_tab(window, session).await
}

#[cfg(any(target_os = "macos", windows))]
pub(super) async fn close_session(app: &tauri::AppHandle, session: &str) -> Result<(), String> {
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("main") {
        desktop::dispatch(window, session.to_owned(), None, Action::Dispose).await?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows))]
pub(super) fn account_changed(app: &tauri::AppHandle) {
    desktop::account_changed(app);
}

// The command retains its input schema on platforms without the native panel.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
#[derive(Clone, Copy, Deserialize)]
pub(crate) struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum Action {
    Mount,
    View,
    Layout {
        bounds: Option<Bounds>,
    },
    Open {
        url: String,
        #[serde(default)]
        allow_development: bool,
        #[serde(default)]
        new_tab: bool,
    },
    Artifact {
        path: String,
    },
    New,
    Select {
        id: u32,
    },
    Close {
        id: u32,
    },
    Back,
    Forward,
    Reload,
    Find {
        query: String,
        #[serde(default)]
        backwards: bool,
    },
    Zoom {
        scale: f64,
    },
    Device {
        viewport: Option<DeviceViewport>,
    },
    Capture,
    EditBegin,
    EditSelection,
    EditEnd,
    EditSave {
        token: String,
        change: ArtifactChange,
    },
    EditUndo,
    EditRedo,
    ClearBrowsingData,
    OpenDownloads,
    RevokeDevelopment,
    Dispose,
}

impl Action {
    #[cfg(any(target_os = "macos", windows))]
    fn requires_user_control(&self) -> bool {
        !matches!(
            self,
            Self::Mount
                | Self::View
                | Self::Layout { .. }
                | Self::Dispose
                | Self::Capture
                | Self::OpenDownloads
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactChange {
    text: Option<String>,
    color: Option<String>,
    font_size: Option<u32>,
    bold: Option<bool>,
    image: Option<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(crate) struct DeviceViewport {
    width: u32,
    height: u32,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    tabs: Vec<TabView>,
    active_id: Option<u32>,
    can_go_back: bool,
    can_go_forward: bool,
    development_endpoints: Vec<String>,
    allow_private_network: bool,
    input_locked: bool,
    zoom: Option<f64>,
    device: Option<DeviceViewport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    find_match: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    history: Vec<HistoryEntry>,
    can_clear_browsing_data: bool,
    profile_tab_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    selection: Option<serde_json::Value>,
}

#[derive(Clone, Serialize, Deserialize)]
struct HistoryEntry {
    url: String,
    title: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TabView {
    id: u32,
    title: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_path: Option<String>,
    can_undo: bool,
    can_redo: bool,
}

#[tauri::command]
pub(crate) async fn browser_panel(
    window: WebviewWindow,
    session_id: String,
    owner: String,
    action: Action,
) -> Result<View, String> {
    if window.label() != "main"
        || owner.len() > 100
        || session_id.is_empty()
        || session_id.len() > 256
    {
        return Err("无效的网页面板会话".into());
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        if matches!(&action, Action::New | Action::Open { .. }) && !web_available() {
            return Err("此系统版本不支持内置网页浏览，请在系统浏览器中打开网址".into());
        }
        let session = match super::SERVICE.get() {
            Some(service) if action.requires_user_control() => {
                Some(service.manager.session(&session_id).await?)
            }
            _ => None,
        };
        let _user_control = if let Some(session) = &session {
            let guard = session
                .execution_gate
                .try_write()
                .map_err(|_| "网页操作正在进行，请先暂停 Agent")?;
            if session.task.lock().unwrap().authorized
                && !session
                    .extension_requested
                    .load(std::sync::atomic::Ordering::Acquire)
                && session.browser_source(&session_id) == super::TaskBrowserSource::Panel
            {
                return Err("Agent 正在操作网页，请先暂停并接手".into());
            }
            Some(guard)
        } else {
            None
        };
        let allow_private = session.as_ref().is_some_and(|session| {
            !session
                .extension_requested
                .load(std::sync::atomic::Ordering::Acquire)
                && session.allows_private_network()
        });
        desktop::dispatch_for_profile(
            window,
            session_id,
            Some(owner),
            action,
            desktop::BrowserProfile::current().await,
            allow_private,
        )
        .await
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (session_id, owner, action);
        Err("当前桌面系统不支持内嵌网页，请用系统浏览器打开".into())
    }
}

#[cfg(any(target_os = "macos", windows))]
mod desktop {
    use super::{Action, Bounds, DeviceViewport, HistoryEntry, TabView, View};
    use crate::automation::browser::parse_navigation_url;
    use crate::automation::network_proxy::{
        development_url_endpoint, resolve_allowed_destinations, BrowserNetworkProxy,
    };
    use crate::automation::{browser::BrowserTarget, native_browser_engine};
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::borrow::Cow;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::{Component, Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tauri::{Manager, WebviewWindow};
    use tokio::sync::{oneshot, OnceCell};
    use url::Url;
    use wry::dpi::{LogicalPosition, LogicalSize};
    use wry::{NewWindowResponse, Rect, WebView, WebViewBuilder};

    thread_local! {
        static PANELS: RefCell<HashMap<String, Panel>> = RefCell::new(HashMap::new());
        static OWNER: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
        static VIEWPORT: RefCell<Option<Rect>> = const { RefCell::new(None) };
        static CLEAR_VIEW: RefCell<Option<(Panel, tempfile::TempDir)>> = const { RefCell::new(None) };
    }

    const ARTIFACT_SCHEME: &str = "echo-preview";
    const ARTIFACT_ORIGIN: &str = "echo-preview://page/";
    const MAX_ASSET_BYTES: u64 = 32 * 1024 * 1024;
    const ARTIFACT_CSP: &str = "default-src 'self' data: blob:; script-src 'self' 'unsafe-inline' blob:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-src 'self' blob:; object-src 'none'; form-action 'none'; base-uri 'self'";

    pub(super) fn account_changed(app: &tauri::AppHandle) {
        let _ = app.run_on_main_thread(|| {
            OWNER.with_borrow_mut(Option::take);
            VIEWPORT.with_borrow_mut(Option::take);
            let old = PANELS.with_borrow_mut(std::mem::take);
            drop(old);
        });
    }

    static PUBLIC_PROXY: OnceCell<Arc<BrowserNetworkProxy>> = OnceCell::const_new();
    static PROFILE_ACCESS: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());
    static HISTORY_WRITE: Mutex<()> = Mutex::new(());
    static HISTORY_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[derive(Clone)]
    pub(super) struct BrowserProfile {
        key: String,
        dir: PathBuf,
        generation: u64,
    }

    impl BrowserProfile {
        pub(super) async fn current() -> Self {
            let generation = crate::org::knowledge_account_generation();
            let account = crate::org::browser_profile_account().await;
            let home = crate::paths::echo_agent_home_dir();
            let key = format!(
                "{:x}",
                Sha256::digest(
                    format!(
                        "echoagent-browser:{}:{}",
                        home.display(),
                        account.as_deref().unwrap_or("local")
                    )
                    .as_bytes()
                )
            );
            let dir = home.join("automation").join("browser-panel").join(&key);
            Self {
                key,
                dir,
                generation,
            }
        }

        fn scope(&self, session: &str) -> String {
            format!("{}:{}:{session}", self.key, self.generation)
        }

        fn check(&self) -> Result<(), String> {
            if self.generation != crate::org::knowledge_account_generation() {
                return Err("账号已切换，请重新打开浏览器".into());
            }
            Ok(())
        }

        fn open(&self) -> Result<(), String> {
            std::fs::create_dir_all(&self.dir).map_err(display)?;
            crate::paths::harden_private_dir(&self.dir)
        }

        fn history(&self) -> Vec<HistoryEntry> {
            let path = self.dir.join("history.json");
            std::fs::symlink_metadata(&path)
                .ok()
                .filter(|metadata| metadata.is_file() && metadata.len() <= 512 * 1024)
                .and_then(|_| std::fs::read(path).ok())
                .and_then(|bytes| serde_json::from_slice::<Vec<HistoryEntry>>(&bytes).ok())
                .unwrap_or_default()
                .into_iter()
                .take(60)
                .collect()
        }

        fn clear_history(&self) -> Result<(), String> {
            let _guard = HISTORY_WRITE
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            self.check()?;
            HISTORY_REVISION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            match std::fs::remove_file(self.dir.join("history.json")) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(display(error)),
            }
        }

        fn record(&self, url: String, title: String) {
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return;
            }
            let Ok(mut safe_url) = parse_navigation_url(&url) else {
                return;
            };
            if url.len() > 4096 {
                return;
            }
            safe_url.set_query(None);
            safe_url.set_fragment(None);
            let url = safe_url.to_string();
            let profile = self.clone();
            let revision = HISTORY_REVISION.load(std::sync::atomic::Ordering::Acquire);
            tauri::async_runtime::spawn_blocking(move || {
                let _guard = HISTORY_WRITE
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if profile.check().is_err()
                    || HISTORY_REVISION.load(std::sync::atomic::Ordering::Acquire) != revision
                {
                    return;
                }
                let mut history = profile.history();
                history.retain(|item| item.url != url);
                history.insert(0, HistoryEntry { url, title });
                history.truncate(60);
                if let Ok(text) = serde_json::to_vec(&history) {
                    let _ = (|| -> Result<(), String> {
                        use std::io::Write;
                        let mut staging =
                            tempfile::NamedTempFile::new_in(&profile.dir).map_err(display)?;
                        staging.write_all(&text).map_err(display)?;
                        staging.as_file().sync_all().map_err(display)?;
                        let path = staging.into_temp_path();
                        crate::paths::replace_file_atomically(
                            path.as_ref(),
                            &profile.dir.join("history.json"),
                        )
                    })();
                }
            });
        }
    }

    struct SourceFile {
        path: PathBuf,
        root: PathBuf,
        url: Url,
        name: String,
    }

    #[derive(Clone)]
    pub(crate) struct SharedBrowser {
        window: WebviewWindow,
        session: String,
        profile: BrowserProfile,
    }

    pub(super) async fn browser(window: WebviewWindow, session: &str) -> SharedBrowser {
        SharedBrowser {
            window,
            session: session.to_owned(),
            profile: BrowserProfile::current().await,
        }
    }

    pub(super) async fn has_tab(window: WebviewWindow, session: &str) -> bool {
        browser(window, session)
            .await
            .active()
            .await
            .is_ok_and(|tab| tab.url != "about:blank")
    }

    impl SharedBrowser {
        pub(super) fn belongs_to_generation(&self, account: Option<u64>) -> bool {
            account == Some(self.profile.generation)
        }
        pub(crate) async fn start(
            window: WebviewWindow,
            session: &str,
            allow_private: bool,
            account_generation: u64,
        ) -> Result<Self, String> {
            let instance = browser(window, session).await;
            if instance.profile.generation != account_generation {
                return Err("账号已切换，请在对话中重新发起网页操作".into());
            }
            let initial = instance.view().await?;
            if initial.1.is_empty() {
                dispatch_for_profile(
                    instance.window.clone(),
                    instance.session.clone(),
                    None,
                    Action::New,
                    instance.profile.clone(),
                    allow_private,
                )
                .await?;
            }
            instance.set_private_network(allow_private).await?;
            Ok(instance)
        }

        pub(crate) async fn action(&self, action: Action) -> Result<View, String> {
            self.profile.check()?;
            dispatch_for_profile(
                self.window.clone(),
                self.session.clone(),
                None,
                action,
                self.profile.clone(),
                false,
            )
            .await
        }

        async fn panel_view(&self) -> Result<View, String> {
            self.profile.check()?;
            let scope = self.profile.scope(&self.session);
            let profile = self.profile.clone();
            ui(&self.window, move || {
                on_panel(&scope, |panel| {
                    Ok(view_for_profile(&profile, panel.as_deref()))
                })
            })
            .await
        }

        async fn clear_browsing_data(&self) -> Result<View, String> {
            let _profile_access = PROFILE_ACCESS
                .try_write()
                .map_err(|_| "网页正在打开，请稍后再清除浏览数据")?;
            if !super::web_available() {
                return Err("当前系统不支持内置浏览器数据清理".into());
            }
            let profile = self.profile.clone();
            let existing = ui(&self.window, move || {
                profile.check()?;
                PANELS.with_borrow(|panels| {
                    let account_panels = panels
                        .values()
                        .filter(|panel| panel.profile.key == profile.key);
                    if account_panels.clone().any(|panel| panel.agent_controlled) {
                        return Err("请先暂停当前账号下正在运行的网页任务，再清除浏览数据".into());
                    }
                    Ok(account_panels
                        .flat_map(|panel| &panel.tabs)
                        .any(|tab| tab.public_profile))
                })
            })
            .await?;
            let proxy = if existing {
                None
            } else {
                Some(public_proxy().await?)
            };
            #[cfg(target_os = "macos")]
            let rule_id = if existing {
                None
            } else {
                Some(macos_proxy::compile(&self.window, None).await?)
            };
            let (sender, receiver) = oneshot::channel();
            let profile = self.profile.clone();
            let parent = self.window.clone();
            ui(&self.window, move || {
                #[cfg(target_os = "macos")]
                let rules = rule_id.and_then(macos_proxy::take);
                profile.check()?;
                if let Some(proxy) = proxy {
                    let directory = tempfile::tempdir().map_err(display)?;
                    let mut panel = Panel::new(
                        Arc::clone(&proxy),
                        proxy,
                        profile,
                        "",
                        directory.path().to_owned(),
                    )?;
                    panel.new_tab(
                        &parent,
                        &Url::parse("about:blank").unwrap(),
                        None,
                        #[cfg(target_os = "macos")]
                        rules,
                    )?;
                    let tab = panel.tab().ok_or("无法打开浏览数据存储")?;
                    native_browser_engine::clear_browsing_data(&tab.webview, sender)?;
                    CLEAR_VIEW.with_borrow_mut(|slot| *slot = Some((panel, directory)));
                    Ok(())
                } else {
                    let scope = PANELS
                        .with_borrow(|panels| {
                            panels
                                .iter()
                                .find(|(_, panel)| {
                                    panel.profile.key == profile.key
                                        && panel.tabs.iter().any(|tab| tab.public_profile)
                                })
                                .map(|(scope, _)| scope.clone())
                        })
                        .ok_or("网页标签已关闭，请重新清除浏览数据")?;
                    on_panel(&scope, |panel| {
                        let tab = panel
                            .and_then(|panel| panel.tabs.iter().find(|tab| tab.public_profile))
                            .ok_or("网页标签已关闭，请重新清除浏览数据")?;
                        native_browser_engine::clear_browsing_data(&tab.webview, sender)
                    })
                }
            })
            .await?;
            let result = tokio::time::timeout(Duration::from_secs(15), receiver)
                .await
                .map_err(|_| "清除浏览器数据超时".to_owned())
                .and_then(|value| value.map_err(|_| "清除浏览器数据已取消".to_owned()))
                .and_then(|value| value);
            let profile = self.profile.clone();
            ui(&self.window, move || {
                let temporary = CLEAR_VIEW.with_borrow_mut(Option::take);
                drop(temporary);
                result?;
                profile.check()?;
                let scopes = PANELS.with_borrow(|panels| {
                    panels
                        .iter()
                        .filter(|(_, panel)| panel.profile.key == profile.key)
                        .map(|(scope, _)| scope.clone())
                        .collect::<Vec<_>>()
                });
                for scope in scopes {
                    on_panel(&scope, |panel| {
                        if let Some(panel) = panel {
                            panel.close_public_tabs();
                            panel.layout()?;
                        }
                        Ok(())
                    })?;
                }
                profile.clear_history()
            })
            .await?;
            self.panel_view().await
        }

        pub(crate) async fn view(&self) -> Result<(Option<String>, Vec<BrowserTarget>), String> {
            self.profile.check()?;
            let session = self.profile.scope(&self.session);
            ui(&self.window, move || {
                on_panel(&session, |panel| {
                    let Some(panel) = panel else {
                        return Ok((None, Vec::new()));
                    };
                    let tabs = panel
                        .tabs
                        .iter()
                        .map(|tab| BrowserTarget {
                            id: tab.id.to_string(),
                            title: tab
                                .title
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .clone(),
                            url: tab.webview.url().unwrap_or_else(|_| "about:blank".into()),
                            target_type: "page".into(),
                            websocket_debugger_url: String::new(),
                        })
                        .collect();
                    Ok((panel.active.map(|id| id.to_string()), tabs))
                })
            })
            .await
        }

        pub(crate) async fn active(&self) -> Result<BrowserTarget, String> {
            let (id, tabs) = self.view().await?;
            tabs.into_iter()
                .find(|target| Some(&target.id) == id.as_ref())
                .ok_or("网页标签已关闭".into())
        }

        async fn edit_artifact(&self, id: &str, action: Action) -> Result<View, String> {
            use super::artifact_editor;
            let scope = self.profile.scope(&self.session);
            let expected = id.to_owned();
            let (path, edits) = ui(&self.window, move || {
                on_panel(&scope, |panel| {
                    let tab = panel
                        .and_then(|p| p.tab())
                        .filter(|tab| tab.id.to_string() == expected)
                        .ok_or("网页标签已切换")?;
                    Ok((
                        tab.artifact.clone().ok_or("只有本地 HTML 产物可以编辑")?,
                        Arc::clone(&tab.edits),
                    ))
                })
            })
            .await?;
            let name = path.to_str().ok_or("HTML 文件路径无效")?;
            if artifact_source(&self.window, &self.session, name)?.path != path {
                return Err("HTML 文件路径已变化，请重新打开产物".into());
            }
            let target = self.active().await?;
            let expected_name = path.file_name().and_then(|name| name.to_str());
            let loaded = Url::parse(&target.url).ok().and_then(|url| {
                is_artifact_url(&target.url).then(|| url.path().trim_start_matches('/').to_owned())
            });
            if target.id != id
                || loaded
                    .as_deref()
                    .and_then(|value| urlencoding::decode(value).ok())
                    .as_deref()
                    != expected_name
            {
                return Err("请返回当前 HTML 产物后再编辑".into());
            }
            let selection = match action {
                Action::EditBegin => {
                    self.artifact_script(id, serde_json::json!({"kind":"start"}))
                        .await?
                }
                Action::EditSelection => {
                    self.artifact_script(id, serde_json::json!({"kind":"selection"}))
                        .await?
                }
                Action::EditEnd => {
                    self.artifact_script(id, serde_json::json!({"kind":"stop"}))
                        .await?
                }
                Action::EditSave { token, change } => {
                    artifact_editor::validate(&change)?;
                    if token.is_empty() || token.len() > 100 {
                        return Err("请重新选择要修改的元素".into());
                    }
                    let lock = crate::document_workspace::document_lock(&path)?;
                    let _file = lock.lock().await;
                    let mut history = edits.lock().await;
                    let before = artifact_editor::read(&path)?;
                    let result = self
                        .artifact_script(
                            id,
                            serde_json::json!({
                                "kind":"apply","source":before,"token":token,"change":change
                            }),
                        )
                        .await?;
                    let after = result.as_str().ok_or("未生成有效的 HTML 修改")?.to_owned();
                    if self.active().await?.id != id
                        || artifact_source(&self.window, &self.session, name)?.path != path
                    {
                        return Err("网页标签或文件已变化，请重新打开".into());
                    }
                    if before != after {
                        artifact_editor::write(&path, &before, &after)?;
                        history.push(before, after);
                    }
                    drop(history);
                    self.reload_artifact(id).await?;
                    Value::Null
                }
                Action::EditUndo | Action::EditRedo => {
                    let lock = crate::document_workspace::document_lock(&path)?;
                    let _file = lock.lock().await;
                    let mut history = edits.lock().await;
                    if artifact_source(&self.window, &self.session, name)?.path != path {
                        return Err("HTML 文件路径已变化，请重新打开产物".into());
                    }
                    history.apply(&path, matches!(action, Action::EditRedo))?;
                    drop(history);
                    self.reload_artifact(id).await?;
                    Value::Null
                }
                _ => unreachable!(),
            };
            let mut view = self.panel_view().await?;
            view.selection = (!selection.is_null()).then_some(selection);
            Ok(view)
        }

        async fn artifact_script(&self, id: &str, input: Value) -> Result<Value, String> {
            let program = include_str!("native_panel/artifact_editor.js");
            let expression = format!("({program})({input})");
            let tree = self
                .call(Some(id), "Page.getFrameTree", serde_json::json!({}))
                .await?;
            let world = self.call(Some(id), "Page.createIsolatedWorld", serde_json::json!({
                "frameId":tree["frameTree"]["frame"]["id"],"worldName":"EchoAgentBrowserInteraction",
            })).await?;
            let result = self.call(Some(id), "Runtime.evaluate", serde_json::json!({
                "expression":expression,"returnByValue":true,"awaitPromise":true,"contextId":world["executionContextId"],
            })).await?;
            if result["exceptionDetails"].is_object() {
                return Err(result["exceptionDetails"]["exception"]["description"]
                    .as_str()
                    .unwrap_or("HTML 产物编辑失败")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned());
            }
            if let Some(error) = result["result"]["value"]["error"].as_str() {
                return Err(error.to_owned());
            }
            Ok(result["result"]["value"].clone())
        }

        async fn reload_artifact(&self, id: &str) -> Result<(), String> {
            self.call(
                Some(id),
                "Page.reload",
                serde_json::json!({ "ignoreCache":true }),
            )
            .await?;
            Ok(())
        }

        pub(crate) async fn select(&self, id: &str) -> Result<BrowserTarget, String> {
            let number = id.parse().map_err(|_| "网页标签标识无效")?;
            self.action(Action::Select { id: number }).await?;
            let current = self.active().await?;
            if current.id != id {
                return Err("网页标签已关闭".into());
            }
            Ok(current)
        }

        pub(crate) async fn navigate(
            &self,
            url: &str,
            new_tab: bool,
        ) -> Result<BrowserTarget, String> {
            self.action(Action::Open {
                url: url.to_owned(),
                allow_development: false,
                new_tab,
            })
            .await?;
            self.active().await
        }

        pub(crate) async fn validate_url(&self, raw: &str) -> Result<Url, String> {
            if is_artifact_url(raw) {
                self.profile.check()?;
                let session = self.profile.scope(&self.session);
                let current = ui(&self.window, move || {
                    on_panel(&session, |panel| {
                        Ok(panel
                            .and_then(|panel| panel.tab())
                            .is_some_and(|tab| tab.file_name.is_some()))
                    })
                })
                .await?;
                if current {
                    return Url::parse(raw).map_err(display);
                }
            }
            let url = parse_navigation_url(raw)?;
            let (development, allow_private) = self.network_policy(&url).await?;
            if let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) {
                if !development && !allow_private {
                    resolve_allowed_destinations(host, port, false, true).await?;
                }
            }
            Ok(url)
        }

        async fn network_policy(&self, url: &Url) -> Result<(bool, bool), String> {
            self.profile.check()?;
            let (session, url) = (self.profile.scope(&self.session), url.clone());
            ui(&self.window, move || {
                on_panel(&session, |panel| {
                    let panel = panel.ok_or("网页面板已关闭")?;
                    Ok((
                        panel.proxy.permits_development_url(&url),
                        panel.proxy.allows_private_network(),
                    ))
                })
            })
            .await
        }

        pub(crate) async fn set_private_network(&self, allowed: bool) -> Result<(), String> {
            self.profile.check()?;
            let session = self.profile.scope(&self.session);
            ui(&self.window, move || {
                on_panel(&session, |panel| {
                    if let Some(panel) = panel {
                        if panel.proxy.allows_private_network() != allowed {
                            panel.proxy.set_allow_private_network(allowed);
                            if allowed {
                                for tab in &panel.tabs {
                                    if let Some(url) =
                                        tab.webview.url().ok().and_then(|url| Url::parse(&url).ok())
                                    {
                                        if development_url_endpoint(&url).is_some() {
                                            panel.proxy.allow_development_url(&url)?;
                                        }
                                    }
                                }
                            } else {
                                // Tabs opened under the broader policy can retain
                                // private documents and their in-memory state.
                                panel
                                    .tabs
                                    .retain(|tab| tab.public_profile || tab.artifact.is_some());
                            }
                            if panel.tab().is_none() {
                                panel.active = panel.tabs.last().map(|tab| tab.id);
                            }
                            panel.layout()?;
                        }
                    }
                    Ok(())
                })
            })
            .await
        }

        pub(crate) async fn set_agent_controlled(&self, controlled: bool) -> Result<(), String> {
            let _profile_access = if controlled {
                Some(
                    PROFILE_ACCESS
                        .try_read()
                        .map_err(|_| "正在清除浏览数据，请稍后操作网页")?,
                )
            } else {
                None
            };
            self.profile.check()?;
            let scope = self.profile.scope(&self.session);
            let editing_tab = ui(&self.window, move || {
                on_panel(&scope, |panel| {
                    let Some(panel) = panel else {
                        return Ok(None);
                    };
                    if panel.agent_controlled != controlled {
                        panel.agent_controlled = controlled;
                        if let Err(error) = panel.layout() {
                            panel.agent_controlled = !controlled;
                            let _ = panel.layout();
                            return Err(error);
                        }
                    }
                    Ok(panel
                        .tab()
                        .filter(|tab| controlled && tab.artifact.is_some())
                        .map(|tab| tab.id.to_string()))
                })
            })
            .await?;
            if let Some(id) = editing_tab {
                self.artifact_script(&id, serde_json::json!({"kind":"stop"}))
                    .await?;
            }
            Ok(())
        }

        pub(crate) async fn call(
            &self,
            expected: Option<&str>,
            method: &str,
            params: Value,
        ) -> Result<Value, String> {
            self.profile.check()?;
            let (session, expected, method) = (
                self.profile.scope(&self.session),
                expected.map(str::to_owned),
                method.to_owned(),
            );
            let (sender, receiver) = oneshot::channel();
            ui(&self.window, move || {
                on_panel(&session, |panel| {
                    let tab = panel
                        .and_then(|panel| panel.tab())
                        .ok_or("网页标签已关闭")?;
                    if expected
                        .as_ref()
                        .is_some_and(|expected| tab.id.to_string() != *expected)
                    {
                        return Err("活动网页标签已变化，请重新观察后继续".into());
                    }
                    native_browser_engine::call(&tab.webview, &method, params, sender)
                })
            })
            .await?;
            tokio::time::timeout(Duration::from_secs(12), receiver)
                .await
                .map_err(|_| "内嵌浏览器操作超时")?
                .map_err(|_| "网页操作已取消")?
        }
    }

    struct Tab {
        id: u32,
        webview: WebView,
        title: Arc<Mutex<String>>,
        file_name: Option<String>,
        artifact: Option<PathBuf>,
        edits: Arc<tokio::sync::Mutex<super::artifact_editor::History>>,
        development: Option<(String, u16)>,
        zoom: f64,
        device: Option<DeviceViewport>,
        public_profile: bool,
        opener: Option<u32>,
        _close_handler: Option<native_browser_engine::PopupCloseHandler>,
    }

    #[cfg(target_os = "macos")]
    impl Drop for Tab {
        fn drop(&mut self) {
            native_browser_engine::detach_frames(&self.webview);
        }
    }

    struct Popup {
        tab_id: u32,
        features: wry::NewWindowFeatures,
    }

    struct Panel {
        session: String,
        tabs: Vec<Tab>,
        active: Option<u32>,
        next_id: u32,
        bounds: Option<Rect>,
        agent_controlled: bool,
        proxy: Arc<BrowserNetworkProxy>,
        public_proxy: Arc<BrowserNetworkProxy>,
        profile: BrowserProfile,
        downloads: PathBuf,
        // A network process started with proxy flags must use a different
        // WebView2 user-data directory from the trusted application UI.
        #[cfg(windows)]
        context: wry::WebContext,
        #[cfg(windows)]
        public_context: wry::WebContext,
        #[cfg(windows)]
        _profile: tempfile::TempDir,
    }

    impl Panel {
        fn new(
            proxy: Arc<BrowserNetworkProxy>,
            public_proxy: Arc<BrowserNetworkProxy>,
            profile: BrowserProfile,
            session: &str,
            downloads: PathBuf,
        ) -> Result<Self, String> {
            profile.open()?;
            std::fs::create_dir_all(&downloads).map_err(display)?;
            crate::paths::harden_private_dir(&downloads)?;
            #[cfg(windows)]
            let private_profile = tempfile::tempdir().map_err(display)?;
            Ok(Self {
                session: session.to_owned(),
                proxy,
                public_proxy,
                #[cfg(windows)]
                public_context: wry::WebContext::new(Some(profile.dir.clone())),
                profile,
                downloads,
                tabs: Vec::new(),
                active: None,
                next_id: 0,
                bounds: None,
                agent_controlled: false,
                #[cfg(windows)]
                context: wry::WebContext::new(Some(private_profile.path().to_owned())),
                #[cfg(windows)]
                _profile: private_profile,
            })
        }

        fn tab(&self) -> Option<&Tab> {
            self.tabs.iter().find(|tab| Some(tab.id) == self.active)
        }

        fn close_public_tabs(&mut self) {
            self.tabs.retain(|tab| !tab.public_profile);
            if self.tab().is_none() {
                self.active = self.tabs.last().map(|tab| tab.id);
            }
        }

        fn view(&self) -> View {
            View {
                tabs: self
                    .tabs
                    .iter()
                    .map(|tab| {
                        let url = tab.file_name.as_ref().cloned().unwrap_or_else(|| {
                            tab.webview.url().unwrap_or_else(|_| "about:blank".into())
                        });
                        let title = tab
                            .title
                            .lock()
                            .map(|title| title.clone())
                            .unwrap_or_default();
                        TabView {
                            id: tab.id,
                            url,
                            title,
                            artifact_path: tab
                                .artifact
                                .as_ref()
                                .map(|path| path.to_string_lossy().into_owned()),
                            can_undo: tab.edits.try_lock().is_ok_and(|edits| edits.can_undo()),
                            can_redo: tab.edits.try_lock().is_ok_and(|edits| edits.can_redo()),
                        }
                    })
                    .collect(),
                active_id: self.active,
                can_go_back: self
                    .tab()
                    .is_some_and(|tab| tab.webview.can_go_back().unwrap_or(false)),
                can_go_forward: self
                    .tab()
                    .is_some_and(|tab| tab.webview.can_go_forward().unwrap_or(false)),
                development_endpoints: self.proxy.development_endpoints(),
                allow_private_network: self.proxy.allows_private_network(),
                input_locked: self.agent_controlled,
                zoom: self.tab().map(|tab| tab.zoom),
                device: self.tab().and_then(|tab| tab.device),
                history: self.profile.history(),
                ..View::default()
            }
        }

        fn layout(&self) -> Result<(), String> {
            for tab in &self.tabs {
                let bounds = self.bounds.filter(|_| Some(tab.id) == self.active);
                if let Some(mut bounds) = bounds {
                    if let Some(device) = tab.device {
                        if let (
                            wry::dpi::Position::Logical(position),
                            wry::dpi::Size::Logical(size),
                        ) = (bounds.position, bounds.size)
                        {
                            let width = size.width.min(f64::from(device.width));
                            let height = size.height.min(f64::from(device.height));
                            bounds.position = LogicalPosition::new(
                                position.x + (size.width - width) / 2.0,
                                position.y,
                            )
                            .into();
                            bounds.size = LogicalSize::new(width, height).into();
                        }
                    }
                    tab.webview.set_bounds(bounds).map_err(display)?;
                }
                tab.webview
                    .set_visible(bounds.is_some() && !self.agent_controlled)
                    .map_err(display)?;
            }
            Ok(())
        }

        fn new_tab(
            &mut self,
            window: &WebviewWindow,
            url: &Url,
            file: Option<SourceFile>,
            #[cfg(target_os = "macos")] rules: Option<macos_proxy::RuleList>,
        ) -> Result<(), String> {
            self.create_tab(
                window,
                url,
                file,
                None,
                #[cfg(target_os = "macos")]
                rules,
            )?;
            Ok(())
        }

        fn create_tab(
            &mut self,
            window: &WebviewWindow,
            url: &Url,
            file: Option<SourceFile>,
            popup: Option<Popup>,
            #[cfg(target_os = "macos")] rules: Option<macos_proxy::RuleList>,
        ) -> Result<&WebView, String> {
            if self.tabs.len() >= 24 {
                return Err("已打开 24 个网页标签，请关闭一些标签后重试".into());
            }
            let title = Arc::new(Mutex::new(String::new()));
            let pending_title = Arc::clone(&title);
            let downloads = self.downloads.clone();
            let artifact = file.is_some();
            let (opener, platform) = match popup {
                Some(popup) => {
                    let opener = self
                        .tabs
                        .iter()
                        .find(|tab| tab.id == popup.tab_id)
                        .ok_or("网页弹窗的来源标签已关闭")?;
                    (Some(opener), Some(popup.features.opener))
                }
                None => (None, None),
            };
            let development = opener.map_or_else(
                || development_url_endpoint(url),
                |tab| tab.development.clone(),
            );
            let public_profile = opener.map_or_else(
                || !artifact && development.is_none() && !self.proxy.allows_private_network(),
                |tab| tab.public_profile,
            );
            let opener_id = opener.map(|tab| tab.id);
            let id = self.next_id;
            self.next_id += 1;
            let proxy = if public_profile {
                &self.public_proxy
            } else {
                &self.proxy
            };
            let address = proxy.address();
            let history_profile = self.profile.clone();
            let history_title = Arc::clone(&title);
            let popup_window = window.clone();
            let popup_profile = self.profile.clone();
            let popup_scope = self.profile.scope(&self.session);
            let popup_time = Mutex::new(None::<std::time::Instant>);
            let popup_development = development.clone();
            let popup_proxy = Arc::clone(proxy);
            #[cfg(target_os = "macos")]
            let builder = {
                use objc2::MainThreadMarker;
                use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
                use wry::WebViewBuilderExtMacos;
                let config = if let Some(platform) = platform {
                    platform.target_configuration
                } else if file.is_none() {
                    macos_proxy::configuration(
                        address,
                        rules.ok_or("缺少网页网络隔离规则")?,
                        public_profile.then_some(&self.profile),
                    )?
                } else {
                    let marker = MainThreadMarker::new().ok_or("网页视图必须在主线程创建")?;
                    let config = unsafe { WKWebViewConfiguration::new(marker) };
                    unsafe {
                        config.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(
                            marker,
                        ))
                    };
                    config
                };
                native_browser_engine::configure_frames(&config)?;
                WebViewBuilder::new().with_webview_configuration(config)
            };
            #[cfg(windows)]
            let builder = WebViewBuilder::new_with_web_context(if public_profile {
                &mut self.public_context
            } else {
                &mut self.context
            });

            let mut builder = builder
                .with_visible(false)
                .with_focused(false)
                .with_incognito(!public_profile)
                .with_devtools(false)
                .with_new_window_req_handler(move |raw, features| {
                    let url = if raw.is_empty() || raw == "about:blank" {
                        Url::parse("about:blank").map_err(display)
                    } else {
                        parse_navigation_url(&raw)
                    };
                    let Ok(url) = url else {
                        return NewWindowResponse::Deny;
                    };
                    // Popups cannot grant access to a different local service.
                    if artifact
                        || popup_profile.check().is_err()
                        || development_url_endpoint(&url).is_some_and(|endpoint| {
                            Some(endpoint) != popup_development
                                || !popup_proxy.permits_development_url(&url)
                        })
                    {
                        return NewWindowResponse::Deny;
                    }
                    let Ok(mut last) = popup_time.lock() else {
                        return NewWindowResponse::Deny;
                    };
                    if last.is_some_and(|time| time.elapsed() < Duration::from_millis(600)) {
                        return NewWindowResponse::Deny;
                    }
                    *last = Some(std::time::Instant::now());
                    drop(last);
                    let opened = on_panel(&popup_scope, |panel| {
                        let panel = panel.ok_or("网页弹窗的会话已关闭")?;
                        let popup = Popup {
                            tab_id: id,
                            features,
                        };
                        let webview = panel.create_tab(
                            &popup_window,
                            &url,
                            None,
                            Some(popup),
                            #[cfg(target_os = "macos")]
                            None,
                        )?;
                        let binding = native_browser_engine::bind_popup(webview);
                        panel.bounds = current_bounds(&popup_scope);
                        panel.layout()?;
                        Ok(binding)
                    });
                    match opened {
                        Ok(binding) => binding,
                        Err(error) => {
                            tracing::debug!(%error, "could not attach a webpage popup");
                            NewWindowResponse::Deny
                        }
                    }
                })
                .with_download_started_handler(move |url, destination| {
                    if artifact || parse_navigation_url(&url).is_err() {
                        return false;
                    }
                    let proposed = destination
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("下载文件");
                    let safe: String = proposed
                        .trim_matches(['.', ' '])
                        .chars()
                        .filter(|ch| !ch.is_control() && !['/', '\\', ':'].contains(ch))
                        .take(140)
                        .collect();
                    let name = if safe.is_empty() {
                        "下载文件"
                    } else {
                        &safe
                    };
                    let first = downloads.join(name);
                    *destination = if first.exists() || first.is_symlink() {
                        downloads.join(format!("{}-{name}", uuid::Uuid::now_v7()))
                    } else {
                        first
                    };
                    true
                })
                .with_on_page_load_handler(move |event, url| {
                    if matches!(event, wry::PageLoadEvent::Finished) && public_profile {
                        let title = history_title
                            .lock()
                            .map(|title| title.clone())
                            .unwrap_or_default();
                        history_profile.record(url, title);
                    }
                })
                .with_document_title_changed_handler(move |value| {
                    if let Ok(mut title) = pending_title.lock() {
                        *title = value.chars().take(256).collect();
                    }
                });
            builder = builder.with_bounds(self.bounds.unwrap_or(Rect {
                position: LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(1024, 768).into(),
            }));

            let artifact_path = file.as_ref().map(|file| file.path.clone());
            let file_name = if let Some(file) = file {
                builder = builder
                    .with_custom_protocol(ARTIFACT_SCHEME.into(), move |_, req| {
                        serve_asset(&file.root, req)
                    })
                    .with_navigation_handler(|url| url == "about:blank" || is_artifact_url(&url));
                Some(file.name)
            } else {
                let proxy = Arc::clone(proxy);
                let allowed = development.clone();
                builder = builder.with_navigation_handler(move |raw| {
                    raw == "about:blank"
                        || parse_navigation_url(&raw).is_ok_and(|url| {
                            development_url_endpoint(&url).is_none_or(|endpoint| {
                                allowed.as_ref() == Some(&endpoint)
                                    && proxy.permits_development_url(&url)
                            })
                        })
                });
                None
            };

            #[cfg(windows)]
            let builder = {
                use wry::WebViewBuilderExtWindows;
                if let Some(platform) = platform {
                    builder.with_environment(platform.environment)
                } else {
                    builder.with_additional_browser_args(format!(
                        "--proxy-server=http://{} --proxy-bypass-list=<-loopback> --disable-quic --force-webrtc-ip-handling-policy=disable_non_proxied_udp",
                        address
                    ))
                }
            };

            // Wry translates its custom scheme for WebView2 only on the
            // builder's initial navigation. On macOS register frames first.
            #[cfg(windows)]
            let builder = if artifact {
                builder.with_url(url.as_str())
            } else {
                builder
            };
            let webview = builder.build_as_child(window).map_err(display)?;
            let close_handler = if opener_id.is_some() {
                let window = window.clone();
                let scope = self.profile.scope(&self.session);
                Some(native_browser_engine::watch_popup_close(
                    &webview,
                    move || {
                        let window = window.clone();
                        let scope = scope.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = ui(&window, move || {
                                on_panel(&scope, |panel| {
                                    if let Some(panel) = panel {
                                        panel.close_tab(id);
                                        panel.layout()?;
                                    }
                                    Ok(())
                                })
                            })
                            .await;
                        });
                    },
                )?)
            } else {
                None
            };
            #[cfg(target_os = "macos")]
            native_browser_engine::attach_frames(&webview);
            if opener_id.is_none() && !(cfg!(windows) && artifact) {
                if let Err(error) = webview.load_url(url.as_str()) {
                    #[cfg(target_os = "macos")]
                    native_browser_engine::detach_frames(&webview);
                    return Err(display(error));
                }
            }
            self.tabs.push(Tab {
                id,
                title,
                webview,
                file_name,
                artifact: artifact_path,
                edits: Default::default(),
                development,
                zoom: 1.0,
                device: None,
                public_profile,
                opener: opener_id,
                _close_handler: close_handler,
            });
            self.active = Some(id);
            Ok(&self.tabs.last().expect("new tab was inserted").webview)
        }

        fn close_tab(&mut self, id: u32) {
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                let opener = self.tabs.remove(index).opener;
                if self.active == Some(id) {
                    self.active = opener
                        .filter(|id| self.tabs.iter().any(|tab| tab.id == *id))
                        .or_else(|| {
                            self.tabs
                                .get(index)
                                .or_else(|| self.tabs.last())
                                .map(|tab| tab.id)
                        });
                }
            }
        }
    }

    async fn public_proxy() -> Result<Arc<BrowserNetworkProxy>, String> {
        PUBLIC_PROXY
            .get_or_try_init(|| async { BrowserNetworkProxy::start(false).await.map(Arc::new) })
            .await
            .cloned()
    }

    fn view_for_profile(profile: &BrowserProfile, panel: Option<&Panel>) -> View {
        let mut view = panel.map(Panel::view).unwrap_or_else(|| View {
            history: profile.history(),
            ..View::default()
        });
        let open_tabs = PANELS.with_borrow(|panels| {
            panels
                .values()
                .filter(|other| other.profile.key == profile.key)
                .flat_map(|other| &other.tabs)
                .filter(|tab| tab.public_profile)
                .count()
        });
        let local_tabs = panel
            .map(|panel| panel.tabs.iter().filter(|tab| tab.public_profile).count())
            .unwrap_or_default();
        view.profile_tab_count = open_tabs + local_tabs;
        view.can_clear_browsing_data = super::web_available() && profile.dir.is_dir();
        view
    }

    pub(super) async fn dispatch(
        window: WebviewWindow,
        session_id: String,
        owner: Option<String>,
        action: Action,
    ) -> Result<View, String> {
        let profile = BrowserProfile::current().await;
        dispatch_for_profile(window, session_id, owner, action, profile, false).await
    }

    pub(super) async fn dispatch_for_profile(
        window: WebviewWindow,
        session: String,
        owner: Option<String>,
        action: Action,
        profile: BrowserProfile,
        allow_private: bool,
    ) -> Result<View, String> {
        profile.check()?;
        let session_id = profile.scope(&session);
        if matches!(
            action,
            Action::Find { .. }
                | Action::Capture
                | Action::ClearBrowsingData
                | Action::EditBegin
                | Action::EditSelection
                | Action::EditEnd
                | Action::EditSave { .. }
                | Action::EditUndo
                | Action::EditRedo
        ) {
            let stamp = (session_id.clone(), owner.clone());
            let active = ui(&window, move || {
                Ok(OWNER.with_borrow(|current| {
                    current.as_ref().is_some_and(|(session, owner)| {
                        session == &stamp.0 && Some(owner) == stamp.1.as_ref()
                    })
                }))
            })
            .await?;
            if !active {
                return Err("当前网页面板已关闭".into());
            }
            let browser = SharedBrowser {
                window,
                session,
                profile,
            };
            if matches!(action, Action::ClearBrowsingData) {
                return browser.clear_browsing_data().await;
            }
            let id = browser.active().await?.id;
            if !matches!(action, Action::Find { .. } | Action::Capture) {
                return browser.edit_artifact(&id, action).await;
            }
            let payload = match &action {
                Action::Find { query, backwards } => {
                    if query.len() > 4096 {
                        return Err("查找文字过长".into());
                    }
                    let query = serde_json::to_string(query).map_err(display)?;
                    let expression = format!("(() => {{const q={query};if(!q){{getSelection()?.removeAllRanges();return false;}}return window.find(q,false,{backwards},true,false,false,false);}})()");
                    browser
                        .call(
                            Some(&id),
                            "Runtime.evaluate",
                            serde_json::json!({
                                "expression":expression,"returnByValue":true,
                            }),
                        )
                        .await?
                }
                Action::Capture => {
                    browser
                        .call(
                            Some(&id),
                            "Page.captureScreenshot",
                            serde_json::json!({
                                "format":"png","fromSurface":true,"captureBeyondViewport":false
                            }),
                        )
                        .await?
                }
                _ => unreachable!(),
            };
            let mut view = browser.panel_view().await?;
            if matches!(action, Action::Capture) {
                view.image = payload["data"].as_str().map(str::to_owned);
                if view.image.is_none() {
                    return Err("网页截图未生成".into());
                }
            } else {
                view.find_match = payload["result"]["value"].as_bool();
            }
            return Ok(view);
        }
        let may_create = matches!(
            &action,
            Action::Open { .. } | Action::Artifact { .. } | Action::New
        );
        let _profile_access = if may_create {
            Some(
                PROFILE_ACCESS
                    .try_read()
                    .map_err(|_| "正在清除浏览数据，请稍后打开网页")?,
            )
        } else {
            None
        };
        let current = if may_create {
            let session_check = session_id.clone();
            ui(&window, move || {
                Ok(PANELS.with_borrow(|panels| {
                    panels
                        .get(&session_check)
                        .map(|panel| Arc::clone(&panel.proxy))
                }))
            })
            .await?
        } else {
            None
        };
        let proxy = match (may_create, current) {
            (true, None) => Some(Arc::new(BrowserNetworkProxy::start(allow_private).await?)),
            (_, current) => current,
        };

        let url = if let Action::Open {
            url,
            allow_development,
            ..
        } = &action
        {
            let url = parse_navigation_url(url)?;
            let proxy = proxy.as_ref().ok_or("网页面板尚未初始化")?;
            if development_url_endpoint(&url).is_some() {
                if *allow_development || proxy.allows_private_network() {
                    proxy.allow_development_url(&url)?;
                } else if !proxy.permits_development_url(&url) {
                    return Err("请先确认此本地服务的端口".into());
                }
            } else if let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) {
                // Early failure with the same resolver that checks redirected
                // requests and subresources at the proxy.
                resolve_allowed_destinations(host, port, proxy.allows_private_network(), true)
                    .await?;
            }
            Some(url)
        } else {
            None
        };
        let source = match &action {
            Action::Artifact { path } => Some(artifact_source(&window, &session, path)?),
            _ => None,
        };
        #[cfg(target_os = "macos")]
        let rule_id = if matches!(&action, Action::Open { .. } | Action::New) {
            Some(macos_proxy::compile(&window, url.as_ref()).await?)
        } else {
            None
        };

        let public_proxy = if may_create {
            Some(public_proxy().await?)
        } else {
            None
        };
        let parent = window.clone();
        ui(&window, move || {
            #[cfg(target_os = "macos")]
            let rules = rule_id.and_then(macos_proxy::take);
            profile.check()?;
            if may_create {
                if let Some(owner) = &owner {
                    OWNER.with_borrow_mut(|current| {
                        if current.is_none() {
                            *current = Some((session_id.clone(), owner.clone()));
                        }
                    });
                }
            }
            let manages_panel = OWNER.with_borrow(|current| {
                current.as_ref().is_some_and(|(scope, id)| {
                    scope == &session_id && owner.as_ref().is_none_or(|owner| owner == id)
                })
            });
            if matches!(&action, Action::Mount) {
                let owner = owner.ok_or("网页面板缺少窗口标识")?;
                if !manages_panel {
                    let previous = OWNER
                        .with_borrow_mut(|current| current.replace((session_id.clone(), owner)));
                    VIEWPORT.with_borrow_mut(Option::take);
                    if let Some((previous, _)) = previous {
                        on_panel(&previous, |panel| {
                            if let Some(panel) = panel {
                                panel.bounds = None;
                                panel.layout()?;
                            }
                            Ok(())
                        })?;
                    }
                }
                return on_panel(&session_id, |panel| {
                    Ok(view_for_profile(&profile, panel.as_deref()))
                });
            }
            if owner.is_some() && !manages_panel {
                return Ok(View::default());
            }
            if matches!(&action, Action::Dispose) {
                if manages_panel {
                    OWNER.with_borrow_mut(Option::take);
                    VIEWPORT.with_borrow_mut(Option::take);
                }
                if owner.is_none() {
                    let removed = PANELS.with_borrow_mut(|panels| panels.remove(&session_id));
                    drop(removed);
                } else {
                    on_panel(&session_id, |panel| {
                        if let Some(panel) = panel {
                            panel.bounds = None;
                            panel.layout()?;
                        }
                        Ok(())
                    })?;
                }
                return Ok(View::default());
            }
            if let Action::Layout { bounds } = &action {
                let bounds = bounds.and_then(Bounds::rect);
                VIEWPORT.with_borrow_mut(|viewport| *viewport = bounds);
                return on_panel(&session_id, |panel| {
                    if let Some(panel) = panel {
                        panel.bounds = bounds;
                        panel.layout()?;
                    }
                    Ok(View::default())
                });
            }
            if matches!(&action, Action::View) {
                return on_panel(&session_id, |panel| {
                    Ok(view_for_profile(&profile, panel.as_deref()))
                });
            }
            let mut panel = PANELS.with_borrow_mut(|panels| panels.remove(&session_id));
            if panel.is_none() && may_create {
                let proxy = proxy.ok_or("网页面板尚未初始化")?;
                let public_proxy = public_proxy.ok_or("网页的公共网络环境未初始化")?;
                let downloads =
                    crate::automation::browser::profile_dir(&session)?.join("Downloads");
                panel = Some(Panel::new(
                    proxy,
                    public_proxy,
                    profile.clone(),
                    &session,
                    downloads,
                )?);
            }
            let Some(mut panel) = panel else {
                return Ok(view_for_profile(&profile, None));
            };
            panel.bounds = if manages_panel {
                VIEWPORT.with_borrow(|viewport| *viewport)
            } else {
                None
            };
            let result = (|| {
                match action {
                    Action::Dispose
                    | Action::Mount
                    | Action::Layout { .. }
                    | Action::Find { .. }
                    | Action::EditBegin
                    | Action::EditSelection
                    | Action::EditEnd
                    | Action::EditSave { .. }
                    | Action::EditUndo
                    | Action::EditRedo
                    | Action::ClearBrowsingData
                    | Action::Capture
                    | Action::View => unreachable!(),
                    Action::Open { new_tab, .. } => {
                        let url = url.as_ref().ok_or("网址为空")?;
                        if let Some(tab) = panel.tab().filter(|tab| {
                            !new_tab
                                && tab.file_name.is_none()
                                && tab.development == development_url_endpoint(url)
                                && tab.public_profile
                                    == (development_url_endpoint(url).is_none()
                                        && !panel.proxy.allows_private_network())
                        }) {
                            tab.webview.load_url(url.as_str()).map_err(display)?;
                        } else {
                            panel.new_tab(
                                &parent,
                                url,
                                None,
                                #[cfg(target_os = "macos")]
                                rules,
                            )?;
                        }
                    }
                    Action::Artifact { .. } => {
                        let source = source.ok_or("缺少网页产物")?;
                        let url = source.url.clone();
                        panel.new_tab(
                            &parent,
                            &url,
                            Some(source),
                            #[cfg(target_os = "macos")]
                            None,
                        )?;
                    }
                    Action::New => {
                        panel.new_tab(
                            &parent,
                            &Url::parse("about:blank").unwrap(),
                            None,
                            #[cfg(target_os = "macos")]
                            rules,
                        )?;
                    }
                    Action::Select { id } => {
                        if panel.tabs.iter().any(|tab| tab.id == id) {
                            panel.active = Some(id);
                            panel.layout()?;
                        }
                    }
                    Action::Close { id } => {
                        panel.close_tab(id);
                        panel.layout()?;
                    }
                    Action::Back | Action::Forward | Action::Reload => {
                        if let Some(tab) = panel.tab() {
                            match action {
                                Action::Back => tab.webview.go_back(),
                                Action::Forward => tab.webview.go_forward(),
                                _ => tab.webview.reload(),
                            }
                            .map_err(display)?;
                        }
                    }
                    Action::Zoom { scale } => {
                        if !scale.is_finite() || !(0.5..=2.0).contains(&scale) {
                            return Err("网页缩放需在 50% 到 200% 之间".into());
                        }
                        if let Some(tab) = panel
                            .tabs
                            .iter_mut()
                            .find(|tab| Some(tab.id) == panel.active)
                        {
                            tab.webview.zoom(scale).map_err(display)?;
                            tab.zoom = scale;
                        }
                    }
                    Action::Device { viewport } => {
                        if viewport.is_some_and(|settings| {
                            !(240..=1920).contains(&settings.width)
                                || !(240..=1440).contains(&settings.height)
                        }) {
                            return Err("设备尺寸超出支持范围".into());
                        }
                        if let Some(tab) = panel
                            .tabs
                            .iter_mut()
                            .find(|tab| Some(tab.id) == panel.active)
                        {
                            tab.device = viewport;
                        }
                    }
                    Action::RevokeDevelopment => {
                        panel.proxy.revoke_development();
                        panel.tabs.retain(|tab| tab.development.is_none());
                        if panel.tab().is_none() {
                            panel.active = panel.tabs.last().map(|tab| tab.id);
                        }
                        panel.layout()?;
                    }
                    Action::OpenDownloads => {
                        open::that(&panel.downloads).map_err(display)?;
                    }
                }
                // WebView2 can process another UI event during child creation.
                // Read the current workspace bounds after constructing it.
                panel.bounds = current_bounds(&session_id);
                panel.layout()?;
                Ok(view_for_profile(&profile, Some(&panel)))
            })();
            if panel.profile.check().is_ok() {
                PANELS.with_borrow_mut(|panels| {
                    panels.entry(session_id).or_insert(panel);
                });
            }
            result
        })
        .await
    }

    fn current_bounds(session_id: &str) -> Option<Rect> {
        let visible =
            OWNER.with_borrow(|owner| owner.as_ref().is_some_and(|(id, _)| id == session_id));
        if visible {
            VIEWPORT.with_borrow(|viewport| *viewport)
        } else {
            None
        }
    }

    fn on_panel<T>(
        session_id: &str,
        work: impl FnOnce(Option<&mut Panel>) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut panel = PANELS
            .with_borrow_mut(|panels| panels.remove(session_id))
            .filter(|panel| panel.profile.check().is_ok());
        let result = work(panel.as_mut());
        if let Some(panel) = panel.filter(|panel| panel.profile.check().is_ok()) {
            PANELS.with_borrow_mut(|panels| {
                panels.entry(session_id.to_owned()).or_insert(panel);
            });
        }
        result
    }

    impl Bounds {
        fn rect(self) -> Option<Rect> {
            let Self {
                x,
                y,
                width,
                height,
            } = self;
            if ![x, y, width, height]
                .into_iter()
                .all(|n| n.is_finite() && (0.0..=32_768.0).contains(&n))
                || width < 1.0
                || height < 1.0
            {
                return None;
            }
            Some(Rect {
                position: LogicalPosition::new(x, y).into(),
                size: LogicalSize::new(width, height).into(),
            })
        }
    }

    async fn ui<T: Send + 'static>(
        window: &WebviewWindow,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (sender, receiver) = oneshot::channel();
        window
            .run_on_main_thread(move || {
                let _ = sender.send(work());
            })
            .map_err(display)?;
        receiver
            .await
            .map_err(|_| "网页面板窗口已关闭".to_string())?
    }

    fn artifact_source(
        window: &WebviewWindow,
        session: &str,
        path: &str,
    ) -> Result<SourceFile, String> {
        let app = window.app_handle();
        let workspace = app
            .state::<crate::commands::AppState>()
            .session_workspace(session)?;
        let filesystem = app.state::<crate::shell_fs::FilesystemAccess>();
        let root = filesystem.require_workspace(&workspace.to_string_lossy())?;
        let file = filesystem.require_authorized_file(&root.join(path))?;
        if !file.starts_with(&root)
            || !file.is_file()
            || mime(&file) != Some("text/html; charset=utf-8")
        {
            return Err("请选择当前工作区内的 HTML 网页产物".into());
        }
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("网页文件名无效")?
            .to_owned();
        let mut url = Url::parse(ARTIFACT_ORIGIN).map_err(display)?;
        url.path_segments_mut()
            .map_err(|_| "网页产物地址无效")?
            .pop_if_empty()
            .push(&name);
        Ok(SourceFile {
            root: file.parent().ok_or("网页产物目录无效")?.to_owned(),
            path: file,
            url,
            name,
        })
    }

    fn is_artifact_url(raw: &str) -> bool {
        Url::parse(raw).is_ok_and(|url| {
            (url.scheme() == ARTIFACT_SCHEME && url.host_str() == Some("page"))
                || (cfg!(windows)
                    && url.scheme() == "http"
                    && url.host_str() == Some("echo-preview.page"))
        })
    }

    fn serve_asset(
        root: &Path,
        request: wry::http::Request<Vec<u8>>,
    ) -> wry::http::Response<Cow<'static, [u8]>> {
        use wry::http::Response;
        let read = || -> Option<(&'static str, Vec<u8>)> {
            if request.method() != wry::http::Method::GET
                || !is_artifact_url(&request.uri().to_string())
            {
                return None;
            }
            let decoded = urlencoding::decode(request.uri().path().trim_start_matches('/')).ok()?;
            if decoded.contains(['\\', ':', '\0']) {
                return None;
            }
            let rel = Path::new(decoded.as_ref());
            if !rel.components().all(|part| {
                matches!(part, Component::Normal(name)
                if !name.to_string_lossy().starts_with('.'))
            }) {
                return None;
            }
            let file = dunce::canonicalize(root.join(rel)).ok()?;
            if !file.starts_with(root) {
                return None;
            }
            let mime = mime(&file)?;
            let metadata = file.metadata().ok()?;
            if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
                return None;
            }
            use std::io::Read;
            let mut data = Vec::new();
            std::fs::File::open(&file)
                .ok()?
                .take(MAX_ASSET_BYTES + 1)
                .read_to_end(&mut data)
                .ok()?;
            (data.len() as u64 <= MAX_ASSET_BYTES).then_some((mime, data))
        };
        if let Some((mime, body)) = read() {
            Response::builder()
                .header("Content-Type", mime)
                .header("Content-Security-Policy", ARTIFACT_CSP)
                .header("X-Content-Type-Options", "nosniff")
                .header("Cache-Control", "no-store")
                .body(Cow::Owned(body))
                .unwrap()
        } else {
            Response::builder()
                .status(404)
                .body(Cow::Borrowed(&[][..]))
                .unwrap()
        }
    }

    fn mime(path: &Path) -> Option<&'static str> {
        Some(
            match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
                "html" | "htm" => "text/html; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "js" | "mjs" => "text/javascript; charset=utf-8",
                "json" => "application/json",
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "svg" => "image/svg+xml",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "ico" => "image/x-icon",
                "woff" => "font/woff",
                "woff2" => "font/woff2",
                "ttf" => "font/ttf",
                "wasm" => "application/wasm",
                _ => return None,
            },
        )
    }

    fn display(error: impl std::fmt::Display) -> String {
        error.to_string()
    }

    #[cfg(target_os = "macos")]
    mod macos_proxy {
        use super::{development_url_endpoint, display, BrowserProfile, WebviewWindow};
        use block2::RcBlock;
        use objc2::{
            class,
            rc::Retained,
            runtime::{AnyObject, NSObject},
            sel, MainThreadMarker,
        };
        use objc2_foundation::{NSArray, NSError, NSString, NSUUID};
        use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
        use serde_json::json;
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::ffi::{c_char, c_void, CString};
        use std::net::SocketAddr;
        use std::time::Duration;
        use tokio::sync::oneshot;
        use url::Url;

        pub(super) type RuleList = Retained<AnyObject>;

        thread_local! {
            static COMPILED: RefCell<HashMap<String, RuleList>> = RefCell::new(HashMap::new());
        }

        pub(super) fn take(id: String) -> Option<RuleList> {
            COMPILED.with_borrow_mut(|lists| lists.remove(&id))
        }

        pub(super) async fn compile(
            window: &WebviewWindow,
            url: Option<&Url>,
        ) -> Result<String, String> {
            // CFNetwork bypasses proxies for loopback URLs. Install a rule in
            // the renderer before navigation, including for subresources.
            let mut rules = Vec::new();
            for scheme in ["https?", "wss?"] {
                for host in [
                    r"([^/@]*\.)?localhost\.?[/:]",
                    r"127\.[0-9.]+[/:]",
                    r"0\.[0-9.]+[/:]",
                    r"169\.254\.[0-9.]+[/:]",
                    r"\[::1\][/:]",
                    r"\[::\][/:]",
                    r"\[::ffff:[0-9a-f:.]+\][/:]",
                    r"\[fe[89ab][0-9a-f]:",
                    r"[^/?#]+@",
                ] {
                    rules.push(json!({"trigger":{"url-filter":format!("^{scheme}://{host}")},"action":{"type":"block"}}));
                }
            }
            if let Some(url) = url.filter(|url| development_url_endpoint(url).is_some()) {
                let host = url
                    .host_str()
                    .ok_or("缺少本地预览主机")?
                    .replace('.', r"\.")
                    .replace('[', r"\[")
                    .replace(']', r"\]");
                let port = url
                    .port()
                    .map(|port| format!(":{port}"))
                    .unwrap_or_default();
                let websocket = if url.scheme() == "https" { "wss" } else { "ws" };
                for scheme in [url.scheme(), websocket] {
                    rules.push(json!({"trigger":{"url-filter":format!("^{scheme}://{host}{port}/")},"action":{"type":"ignore-previous-rules"}}));
                }
            }

            let id = uuid::Uuid::now_v7().to_string();
            let key = id.clone();
            let encoded = serde_json::to_string(&rules).map_err(display)?;
            let (sender, receiver) = oneshot::channel();
            window.run_on_main_thread(move || unsafe {
                let store: Retained<AnyObject> = objc2::msg_send![class!(WKContentRuleListStore), defaultStore];
                let identifier = NSString::from_str(&key);
                let source = NSString::from_str(&encoded);
                let sender = RefCell::new(Some(sender));
                let callback = RcBlock::new(move |rule: *mut AnyObject, error: *mut NSError| {
                    if let Some(sender) = sender.borrow_mut().take() {
                        if let Some(rule) = Retained::retain(rule) {
                            if sender.send(Ok(())).is_ok() {
                                COMPILED.with_borrow_mut(|lists| lists.insert(key.clone(), rule));
                            }
                        } else {
                            let detail = error.as_ref().map(|error| error.localizedDescription().to_string()).unwrap_or_default();
                            let _ = sender.send(Err(format!("网页隔离规则无法加载：{detail}")));
                        }
                    }
                });
                let _: () = objc2::msg_send![&*store, compileContentRuleListForIdentifier: &*identifier,
                    encodedContentRuleList: &*source, completionHandler: &*callback];
            }).map_err(display)?;
            tokio::time::timeout(Duration::from_secs(8), receiver)
                .await
                .map_err(|_| "网页隔离规则加载超时")?
                .map_err(|_| "网页隔离规则加载已取消")??;
            Ok(id)
        }

        pub(super) fn configuration(
            address: SocketAddr,
            rules: RuleList,
            profile: Option<&BrowserProfile>,
        ) -> Result<Retained<WKWebViewConfiguration>, String> {
            let unsupported = "系统 WebKit 不支持隔离的网页代理，请用系统浏览器打开";
            let mtm = MainThreadMarker::new().ok_or("网页视图必须在主线程创建")?;
            // The Network framework's proxy functions are available on macOS
            // 14+. Resolve them lazily so older hosts can still open artifacts.
            unsafe {
                let configuration = WKWebViewConfiguration::new(mtm);
                let store = if let Some(profile) = profile {
                    let mut identifier = [0u8; 16];
                    for (offset, slot) in identifier.iter_mut().enumerate() {
                        *slot = u8::from_str_radix(&profile.key[offset * 2..offset * 2 + 2], 16)
                            .map_err(display)?;
                    }
                    WKWebsiteDataStore::dataStoreForIdentifier(&NSUUID::from_bytes(identifier), mtm)
                } else {
                    WKWebsiteDataStore::nonPersistentDataStore(mtm)
                };
                let _: () = objc2::msg_send![&*configuration.userContentController(), addContentRuleList: &*rules];
                let supported: bool =
                    objc2::msg_send![&*store, respondsToSelector: sel!(setProxyConfigurations:)];
                if !supported {
                    return Err(unsupported.into());
                }
                let library = libc::dlopen(
                    c"/System/Library/Frameworks/Network.framework/Network".as_ptr(),
                    libc::RTLD_LAZY | libc::RTLD_LOCAL,
                );
                if library.is_null() {
                    return Err(unsupported.into());
                }
                let result = (|| {
                    let endpoint_fn = libc::dlsym(library, c"nw_endpoint_create_host".as_ptr());
                    let proxy_fn =
                        libc::dlsym(library, c"nw_proxy_config_create_http_connect".as_ptr());
                    if endpoint_fn.is_null() || proxy_fn.is_null() {
                        return Err(unsupported.to_string());
                    }
                    let endpoint_fn: unsafe extern "C" fn(
                        *const c_char,
                        *const c_char,
                    ) -> *mut NSObject = std::mem::transmute(endpoint_fn);
                    let proxy_fn: unsafe extern "C" fn(
                        *const NSObject,
                        *const c_void,
                    ) -> *mut NSObject = std::mem::transmute(proxy_fn);
                    let host = CString::new(address.ip().to_string()).unwrap();
                    let port = CString::new(address.port().to_string()).unwrap();
                    let endpoint = Retained::from_raw(endpoint_fn(host.as_ptr(), port.as_ptr()))
                        .ok_or(unsupported)?;
                    let proxy = Retained::from_raw(proxy_fn(&*endpoint, std::ptr::null()))
                        .ok_or(unsupported)?;
                    let proxies = NSArray::from_retained_slice(&[proxy]);
                    let _: () = objc2::msg_send![&*store, setProxyConfigurations: &*proxies];
                    configuration.setWebsiteDataStore(&store);
                    Ok(())
                })();
                libc::dlclose(library);
                result.map(|()| configuration)
            }
        }
    }
}
