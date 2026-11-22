//! WebKit provides the native frame handle on isolated script messages.
//! The registered channel accepts frame identity alone.
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, MainThreadMarker, MainThreadOnly,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKContentWorld, WKFrameInfo, WKScriptMessage, WKScriptMessageHandler, WKUserContentController,
    WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;

const CHANNEL: &str = "echoAgentFrameRegistration";
const SCRIPT: &str = include_str!("frame_registration.js");
const MAX_FRAMES: usize = 128;
pub(super) const ROOT: u64 = 1;

#[derive(Clone)]
pub(super) struct Frame {
    pub(super) context: u64,
    pub(super) token: String,
    pub(super) path: Vec<u16>,
    pub(super) info: Retained<WKFrameInfo>,
}

#[derive(Default)]
struct Registry {
    next: u64,
    views: HashMap<usize, Vec<Frame>>,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::default();
    pub(super) static WORLD: Retained<WKContentWorld> = unsafe {
        WKContentWorld::worldWithName(&NSString::from_str("EchoAgentBrowserInteraction"),
            MainThreadMarker::new().expect("WebKit scripts run on the main thread"))
    };
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    token: String,
    path: Option<Vec<u16>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EchoAgentWebKitFrameRegistrar"]
    struct Registrar;

    unsafe impl NSObjectProtocol for Registrar {}

    unsafe impl WKScriptMessageHandler for Registrar {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        unsafe fn receive(&self, _: &WKUserContentController, message: &WKScriptMessage) {
            let Some(view) = (unsafe { message.webView() }) else {
                return;
            };
            let key = view_key(&view);
            let body = unsafe { message.body() };
            let Some(body) = body.downcast_ref::<NSString>() else {
                return;
            };
            if body.length() > 512 {
                return;
            }
            let Ok(Registration { token, path }) = serde_json::from_str(&body.to_string()) else {
                return;
            };
            if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return;
            }
            let info = unsafe { message.frameInfo() };
            REGISTRY.with_borrow_mut(|registry| {
                let Some(frames) = registry.views.get_mut(&key) else {
                    return;
                };
                if let Some(path) = path {
                    if path.len() > 16
                        || path.iter().any(|index| *index >= 256)
                        || path.is_empty() != unsafe { info.isMainFrame() }
                    {
                        return;
                    }
                    if let Some(existing) = frames.iter_mut().find(|frame| frame.token == token) {
                        existing.path = path;
                        existing.info = info;
                        return;
                    }
                    frames.retain(|frame| frame.path != path);
                    if frames.len() == MAX_FRAMES {
                        return;
                    }
                    registry.next = registry.next.max(ROOT) + 1;
                    frames.push(Frame {
                        context: registry.next,
                        token,
                        path,
                        info,
                    });
                } else {
                    frames.retain(|frame| frame.token != token);
                }
            });
        }
    }
);

pub(super) fn configure(config: &WKWebViewConfiguration) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("网页视图必须在主线程创建")?;
    let controller = unsafe { config.userContentController() };
    let source = NSString::from_str(SCRIPT);
    // Popups can share the opener's content controller.
    if unsafe { controller.userScripts() }
        .iter()
        .any(|script| unsafe { script.source() } == source)
    {
        return Ok(());
    }
    let registrar: Retained<Registrar> = unsafe { msg_send![Registrar::alloc(mtm), init] };
    let channel = NSString::from_str(CHANNEL);
    WORLD.with(|world| unsafe {
        controller.addScriptMessageHandler_contentWorld_name(
            ProtocolObject::from_ref(&*registrar),
            world,
            &channel,
        );
        let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
            WKUserScript::alloc(mtm),
            &source,
            WKUserScriptInjectionTime::AtDocumentStart,
            false,
            world,
        );
        controller.addUserScript(&script);
    });
    Ok(())
}

fn view_key(view: &WKWebView) -> usize {
    std::ptr::from_ref(view) as usize
}

pub(super) fn attach(view: &WKWebView) {
    REGISTRY.with_borrow_mut(|registry| {
        registry.views.entry(view_key(view)).or_default();
    });
}

pub(super) fn detach(view: &WKWebView) {
    REGISTRY.with_borrow_mut(|registry| {
        registry.views.remove(&view_key(view));
    });
}

pub(super) fn lookup(view: &WKWebView, context: u64) -> Result<Option<Frame>, String> {
    if context == ROOT {
        return Ok(None);
    }
    REGISTRY.with_borrow(|registry| {
        registry
            .views
            .get(&view_key(view))
            .and_then(|frames| {
                frames
                    .iter()
                    .find(|frame| frame.context == context && !frame.path.is_empty())
                    .cloned()
            })
            .map(Some)
            .ok_or_else(|| "网页子框架已变化，请重新观察页面".into())
    })
}

pub(super) fn listed(view: &WKWebView) -> Vec<Value> {
    REGISTRY.with_borrow(|registry| {
        registry
            .views
            .get(&view_key(view))
            .into_iter()
            .flatten()
            .filter(|frame| !frame.path.is_empty())
            .map(|frame| json!({ "frame": { "id": frame.context.to_string() } }))
            .collect()
    })
}

pub(super) fn context_for_path(view: &WKWebView, path: &[u16]) -> Result<u64, String> {
    if path.is_empty() {
        return Ok(ROOT);
    }
    REGISTRY.with_borrow(|registry| {
        registry
            .views
            .get(&view_key(view))
            .into_iter()
            .flatten()
            .find(|frame| frame.path == path)
            .map(|frame| frame.context)
            .ok_or_else(|| "网页子框架位置已变化，请重新观察页面".into())
    })
}
