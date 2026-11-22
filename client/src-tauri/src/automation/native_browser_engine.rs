//! Browser commands for a native child view. The shell never evaluates code in
//! the application's own webview on behalf of a page.
use serde_json::Value;
use tokio::sync::oneshot;
use wry::WebView;

pub(super) type Reply = oneshot::Sender<Result<Value, String>>;

#[cfg(target_os = "macos")]
mod frames;
mod popup;

#[cfg(target_os = "macos")]
pub(super) type PopupCloseHandler = objc2::rc::Retained<popup::CloseHandler>;
#[cfg(windows)]
pub(super) type PopupCloseHandler = ();

pub(super) fn watch_popup_close(
    view: &WebView,
    close: impl Fn() + 'static,
) -> Result<PopupCloseHandler, String> {
    popup::watch_close(view, close)
}

pub(super) fn bind_popup(view: &WebView) -> wry::NewWindowResponse {
    #[cfg(target_os = "macos")]
    use wry::WebViewExtMacOS;
    #[cfg(windows)]
    use wry::WebViewExtWindows;
    #[cfg(target_os = "macos")]
    let webview = view.webview().into_super();
    #[cfg(windows)]
    let webview = view.webview();
    wry::NewWindowResponse::Create { webview }
}

#[cfg(target_os = "macos")]
pub(super) fn configure_frames(
    configuration: &objc2_web_kit::WKWebViewConfiguration,
) -> Result<(), String> {
    frames::configure(configuration)
}

#[cfg(target_os = "macos")]
pub(super) fn attach_frames(view: &WebView) {
    use wry::WebViewExtMacOS;
    frames::attach(&view.webview());
}

#[cfg(target_os = "macos")]
pub(super) fn detach_frames(view: &WebView) {
    use wry::WebViewExtMacOS;
    frames::detach(&view.webview());
}

#[cfg(windows)]
pub(super) fn clear_browsing_data(view: &WebView, reply: Reply) -> Result<(), String> {
    use std::cell::RefCell;
    use webview2_com::{
        ClearBrowsingDataCompletedHandler,
        Microsoft::Web::WebView2::Win32::{ICoreWebView2Profile2, ICoreWebView2_13},
    };
    use windows_core::Interface;
    use wry::WebViewExtWindows;
    let reply = RefCell::new(Some(reply));
    let completion = ClearBrowsingDataCompletedHandler::create(Box::new(move |status| {
        if let Some(reply) = reply.borrow_mut().take() {
            let _ = reply.send(
                status
                    .map(|()| Value::Null)
                    .map_err(|error| error.to_string()),
            );
        }
        Ok(())
    }));
    let clear = || unsafe {
        view.webview()
            .cast::<ICoreWebView2_13>()?
            .Profile()?
            .cast::<ICoreWebView2Profile2>()?
            .ClearBrowsingDataAll(&completion)
    };
    clear().map_err(|error| format!("无法清除内置浏览器数据：{error}"))
}

#[cfg(target_os = "macos")]
pub(super) fn clear_browsing_data(view: &WebView, reply: Reply) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_foundation::NSDate;
    use objc2_web_kit::WKWebsiteDataStore;
    use std::cell::RefCell;
    use wry::WebViewExtMacOS;
    let marker = MainThreadMarker::new().ok_or("浏览器存储必须在主线程清理")?;
    let reply = RefCell::new(Some(reply));
    let completion = block2::RcBlock::new(move || {
        if let Some(reply) = reply.borrow_mut().take() {
            let _ = reply.send(Ok(Value::Null));
        }
    });
    unsafe {
        let store = view.webview().configuration().websiteDataStore();
        store.removeDataOfTypes_modifiedSince_completionHandler(
            &WKWebsiteDataStore::allWebsiteDataTypes(marker),
            &NSDate::dateWithTimeIntervalSince1970(0.0),
            &completion,
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) async fn upload_contents(paths: &[String]) -> Result<Vec<Value>, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Path;
    let paths = paths.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut remaining = 8 * 1024 * 1024;
        let mut files = Vec::with_capacity(paths.len());
        for path in &paths {
            let path = Path::new(path);
            let valid = dunce::canonicalize(path).is_ok_and(|actual| actual == path);
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|_| valid)
                .ok_or("已授权的上传文件路径已变化")?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(|error| format!("无法读取已授权的上传文件：{error}"))?;
            if !file
                .metadata()
                .is_ok_and(|data| data.is_file() && data.len() <= remaining)
            {
                return Err(
                    "内嵌网页一次可选择最多 8 MB 的普通文件。大文件请在网页中手动选择。".into(),
                );
            }
            let mut bytes = Vec::new();
            file.take(remaining + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| format!("无法读取已授权的上传文件：{error}"))?;
            remaining = remaining
                .checked_sub(bytes.len() as u64)
                .ok_or("上传文件总大小超过 8 MB")?;
            files.push(serde_json::json!({"name":name,"base64":STANDARD.encode(&bytes)}));
        }
        Ok(files)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(windows)]
pub(super) fn call(
    view: &WebView,
    method: &str,
    params: Value,
    reply: Reply,
) -> Result<(), String> {
    use std::cell::RefCell;
    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
    use windows_core::HSTRING;
    use wry::WebViewExtWindows;
    let reply = RefCell::new(Some(reply));
    let completion =
        CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |status, raw| {
            if let Some(reply) = reply.borrow_mut().take() {
                let result = status
                    .map_err(|error| error.to_string())
                    .and_then(|()| serde_json::from_str(&raw).map_err(|error| error.to_string()));
                let _ = reply.send(result);
            }
            Ok(())
        }));
    let method = HSTRING::from(method);
    let params = HSTRING::from(params.to_string());
    unsafe {
        view.webview()
            .CallDevToolsProtocolMethod(&method, &params, &completion)
    }
    .map_err(|error| format!("内嵌浏览器命令执行失败：{error}"))
}

#[cfg(target_os = "macos")]
pub(super) fn call(
    view: &WebView,
    method: &str,
    params: Value,
    reply: Reply,
) -> Result<(), String> {
    mac::call(view, method, params, reply)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::frames::{Frame, ROOT, WORLD};
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use block2::RcBlock;
    use objc2::{class, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{NSData, NSError, NSPoint, NSString};
    use serde_json::json;
    use std::cell::RefCell;
    use wry::WebViewExtMacOS;

    fn frame_context(id: &Value) -> Result<u64, String> {
        match id.as_str() {
            Some("main") => Ok(ROOT),
            Some(id) => id.parse().map_err(|_| "网页子框架标识无效".into()),
            None => Err("缺少网页子框架标识".into()),
        }
    }

    fn object_context(params: &Value) -> Result<(u64, String), String> {
        let (context, id) = params["objectId"]
            .as_str()
            .and_then(|id| id.split_once(':'))
            .ok_or("网页对象引用已失效")?;
        Ok((
            context.parse().map_err(|_| "网页对象引用已失效")?,
            serde_json::to_string(id).map_err(|e| e.to_string())?,
        ))
    }

    pub(super) fn call(
        view: &WebView,
        method: &str,
        params: Value,
        reply: Reply,
    ) -> Result<(), String> {
        match method {
            "Page.getFrameTree" => {
                let child_frames = json!(frames::listed(&view.webview()));
                evaluate(
                    view,
                    None,
                    &format!(
                        r#"(() => ({{frameTree:{{
                    frame:{{id:"main",url:location.href,
                        loaderId:globalThis.__echoLoader ||= crypto.randomUUID?.() ?? String(Math.random())}},
                    childFrames:{child_frames}
                }}}}))()"#
                    ),
                    reply,
                    false,
                )
            }
            "Page.createIsolatedWorld" => {
                let context = frame_context(&params["frameId"])?;
                frames::lookup(&view.webview(), context)?;
                finish(reply, Ok(json!({"executionContextId":context})))
            }
            "Echo.framePath" => {
                let context = frame_context(&params["frameId"])?;
                let frame = frames::lookup(&view.webview(), context)?;
                evaluate(
                    view,
                    frame,
                    "globalThis.__echoFrameRegistration?.path()",
                    reply,
                    false,
                )
            }
            "Echo.projectFrame" => {
                let path = serde_json::from_value::<Vec<u16>>(params["parentPath"].clone())
                    .map_err(|_| "网页子框架路径无效")?;
                let index = params["index"]
                    .as_u64()
                    .filter(|value| *value < 256)
                    .ok_or("网页子框架序号无效")?;
                let point = &params["point"];
                let point = if point.is_null() {
                    Value::Null
                } else {
                    let (x, y) = point["x"]
                        .as_f64()
                        .zip(point["y"].as_f64())
                        .ok_or("网页子框架坐标无效")?;
                    json!({"x":x,"y":y})
                };
                let native = view.webview();
                let context = frames::context_for_path(&native, &path)?;
                let frame = frames::lookup(&native, context)?;
                let script = include_str!("native_browser_engine/frame_projection.js");
                let path = json!(path);
                evaluate(
                    view,
                    frame,
                    &format!("({script})({path},{index},{point})"),
                    reply,
                    false,
                )
            }
            "Runtime.evaluate" => {
                let expression = params["expression"].as_str().ok_or("缺少页面表达式")?;
                let context = params["contextId"].as_u64().unwrap_or(ROOT);
                let frame = frames::lookup(&view.webview(), context)?;
                if params["returnByValue"] == false {
                    let source = serde_json::to_string(expression).map_err(|e| e.to_string())?;
                    let expression = format!("(() => {{ const node=(0,eval)({source}); if(!node)return {{}}; const id=String(++globalThis.__echoNextObject|| (globalThis.__echoNextObject=1)); (globalThis.__echoObjects||=new Map()).set(id,node); return {{objectId:'{context}:'+id}}; }})()");
                    evaluate(view, frame, &expression, reply, true)
                } else {
                    evaluate(view, frame, expression, reply, true)
                }
            }
            "Runtime.callFunctionOn" => {
                let (context, id) = object_context(&params)?;
                let function = params["functionDeclaration"]
                    .as_str()
                    .ok_or("缺少内部页面方法")?;
                let arguments = params["arguments"]
                    .as_array()
                    .map(|args| args.iter().map(|arg| &arg["value"]).collect::<Vec<_>>())
                    .unwrap_or_default();
                let expression = format!(
                    "({function}).apply(globalThis.__echoObjects?.get({id}),{arguments})",
                    arguments = serde_json::to_string(&arguments).map_err(|e| e.to_string())?
                );
                evaluate(
                    view,
                    frames::lookup(&view.webview(), context)?,
                    &expression,
                    reply,
                    true,
                )
            }
            "Runtime.releaseObject" => {
                let (context, id) = object_context(&params)?;
                evaluate(
                    view,
                    frames::lookup(&view.webview(), context)?,
                    &format!("globalThis.__echoObjects?.delete({id}) ?? false"),
                    reply,
                    false,
                )
            }
            "Input.insertText" => {
                let text = serde_json::to_string(&params["text"]).map_err(|e| e.to_string())?;
                let reference =
                    serde_json::to_string(&params["echoReference"]).map_err(|e| e.to_string())?;
                let context = if params["echoFrameId"].is_null() {
                    ROOT
                } else {
                    frame_context(&params["echoFrameId"])?
                };
                let frame = frames::lookup(&view.webview(), context)?;
                evaluate(
                    view,
                    frame,
                    &format!(
                        r#"(() => {{
                    let active = document.activeElement;
                    for (let depth=0; depth<16 && active?.shadowRoot?.activeElement; depth++) active=active.shadowRoot.activeElement;
                    if (active?.type==="password") throw new Error("密码字段仅可由用户输入");
                    if ({reference}!==null && active?.getAttribute("data-echo-ref")!=={reference}) throw new Error("网页输入焦点已变化");
                    if (!document.execCommand("insertText",false,{text})) throw new Error("当前控件未接受文本");
                    return true;
                }})()"#
                    ),
                    reply,
                    false,
                )
            }
            "Input.dispatchMouseEvent" => {
                if params["type"] == "mouseWheel" {
                    let dx = params["deltaX"].as_f64().unwrap_or_default();
                    let dy = params["deltaY"].as_f64().unwrap_or_default();
                    let x = params["x"].as_f64().unwrap_or_default();
                    let y = params["y"].as_f64().unwrap_or_default();
                    return evaluate(
                        view,
                        None,
                        &format!(
                            r#"(() => {{
                            for(let node=document.elementFromPoint({x},{y}); node && node!==document.body; node=node.parentElement) {{
                                const style=getComputedStyle(node);
                                const available=(delta,current,span) => delta > 0 ? current < span : delta < 0 && current > 0;
                                const horizontal=/auto|scroll/.test(style.overflowX) && available({dx},node.scrollLeft,node.scrollWidth-node.clientWidth);
                                const vertical=/auto|scroll/.test(style.overflowY) && available({dy},node.scrollTop,node.scrollHeight-node.clientHeight);
                                if(horizontal||vertical) {{
                                    node.scrollBy({{left:{dx},top:{dy},behavior:"instant"}});
                                    return true;
                                }}
                            }}
                            scrollBy({{left:{dx},top:{dy},behavior:"instant"}});
                            return true;
                        }})()"#
                        ),
                        reply,
                        false,
                    );
                }
                finish(reply, mouse(view, &params))
            }
            "Input.dispatchKeyEvent" => finish(reply, key(view, &params)),
            "Page.captureScreenshot" => capture(view, reply),
            "Page.bringToFront" => finish(
                reply,
                view.focus().map(|()| json!({})).map_err(|e| e.to_string()),
            ),
            "Page.navigate" => {
                let url = params["url"].as_str().ok_or("缺少网页地址")?;
                finish(
                    reply,
                    view.load_url(url)
                        .map(|()| json!({"frameId":"main"}))
                        .map_err(|e| e.to_string()),
                )
            }
            "Page.reload" => finish(
                reply,
                view.reload().map(|()| json!({})).map_err(|e| e.to_string()),
            ),
            "DOM.setFileInputFiles" => Err("请在当前网页的文件选择器中选择要上传的文件".into()),
            _ => Err(format!("当前内嵌浏览器不支持内部命令 {method}")),
        }
    }

    fn finish(reply: Reply, result: Result<Value, String>) -> Result<(), String> {
        let _ = reply.send(result);
        Ok(())
    }

    fn evaluate(
        view: &WebView,
        frame: Option<Frame>,
        expression: &str,
        reply: Reply,
        wrapped: bool,
    ) -> Result<(), String> {
        let encoded = serde_json::to_string(expression).map_err(|e| e.to_string())?;
        let guard = frame.as_ref().map(|frame| format!(
            "if(globalThis.__echoFrameRegistration?.token!==\"{}\") throw new Error('网页子框架已导航，请重新观察页面');", frame.token
        )).unwrap_or_default();
        let body = NSString::from_str(&format!(
            "{guard} const expression={encoded}; const value=await (0,eval)(expression); return JSON.stringify(value===undefined?null:value);"
        ));
        let reply = RefCell::new(Some(reply));
        let completion = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let result = unsafe {
                if let Some(error) = error.as_ref() {
                    Err(format!("网页操作失败：{}", error.localizedDescription()))
                } else if let Some(value) = value
                    .as_ref()
                    .and_then(|value| value.downcast_ref::<NSString>())
                {
                    serde_json::from_str(&value.to_string()).map_err(|error| error.to_string())
                } else {
                    Err("网页未返回可读的结果".into())
                }
            };
            if let Some(reply) = reply.borrow_mut().take() {
                let result = result.map(|value: Value| {
                    if wrapped {
                        if value["objectId"].is_string() {
                            json!({"result":value})
                        } else {
                            json!({"result":{"value":value}})
                        }
                    } else {
                        value
                    }
                });
                let _ = reply.send(result);
            }
        });
        WORLD.with(|world| unsafe {
            let nil = std::ptr::null::<AnyObject>();
            let frame = frame.as_ref().map(|frame| std::ptr::from_ref(&*frame.info).cast::<AnyObject>()).unwrap_or(nil);
            let _: () = objc2::msg_send![&*view.webview(), callAsyncJavaScript: &*body,
                arguments: nil, inFrame: frame, inContentWorld: &**world, completionHandler: &*completion];
        });
        Ok(())
    }

    fn mouse(view: &WebView, params: &Value) -> Result<Value, String> {
        let x = params["x"].as_f64().ok_or("网页横坐标无效")?;
        let y = params["y"].as_f64().ok_or("网页纵坐标无效")?;
        let button = params["button"].as_str().unwrap_or("left");
        let phase = params["type"].as_str().ok_or("缺少鼠标动作")?;
        // AppKit event discriminants. Deliver only to this exact child view
        // instead of using a system-wide cursor or the top-level app shell.
        let (event_type, selector) = match (phase, button) {
            ("mousePressed", "left") => (1, objc2::sel!(mouseDown:)),
            ("mouseReleased", "left") => (2, objc2::sel!(mouseUp:)),
            ("mousePressed", "right") => (3, objc2::sel!(rightMouseDown:)),
            ("mouseReleased", "right") => (4, objc2::sel!(rightMouseUp:)),
            ("mousePressed", "middle") => (25, objc2::sel!(otherMouseDown:)),
            ("mouseReleased", "middle") => (26, objc2::sel!(otherMouseUp:)),
            ("mouseMoved", _) if params["buttons"] == 1 => (6, objc2::sel!(mouseDragged:)),
            ("mouseMoved", _) => (5, objc2::sel!(mouseMoved:)),
            _ => return Err("不支持此鼠标动作".into()),
        };
        let child = view.webview();
        let window = view.ns_window();
        unsafe {
            let bounds: objc2_foundation::NSRect = objc2::msg_send![&*child, bounds];
            let zoom: f64 = objc2::msg_send![&*child, pageZoom];
            let x = x * zoom;
            let y = y * zoom;
            if phase != "mouseReleased"
                && (x < 0.0 || y < 0.0 || x >= bounds.size.width || y >= bounds.size.height)
            {
                return Err("操作坐标不在网页可视区内".into());
            }
            let flipped: bool = objc2::msg_send![&*child, isFlipped];
            let point = NSPoint::new(x, if flipped { y } else { bounds.size.height - y });
            let location: NSPoint = objc2::msg_send![&*child, convertPoint: point, toView: std::ptr::null::<AnyObject>()];
            let window_number: isize = objc2::msg_send![&*window, windowNumber];
            let timestamp = event_timestamp();
            let flags = modifier_flags(params["modifiers"].as_u64().unwrap_or_default());
            let clicks = params["clickCount"].as_u64().unwrap_or(0).min(3) as isize;
            let event: Option<Retained<AnyObject>> = objc2::msg_send![class!(NSEvent),
                mouseEventWithType: event_type as usize, location: location, modifierFlags: flags,
                timestamp: timestamp, windowNumber: window_number, context: std::ptr::null::<AnyObject>(),
                eventNumber: 0isize, clickCount: clicks, pressure: if phase == "mousePressed" { 1.0f32 } else { 0.0f32 }
            ];
            let event = event.ok_or("无法创建网页鼠标事件")?;
            let child: &AnyObject = &child;
            if selector == objc2::sel!(mouseDown:) {
                let _: () = objc2::msg_send![child, mouseDown: &*event];
            } else if selector == objc2::sel!(mouseUp:) {
                let _: () = objc2::msg_send![child, mouseUp: &*event];
            } else if selector == objc2::sel!(mouseMoved:) {
                let _: () = objc2::msg_send![child, mouseMoved: &*event];
            } else if selector == objc2::sel!(mouseDragged:) {
                let _: () = objc2::msg_send![child, mouseDragged: &*event];
            } else if selector == objc2::sel!(rightMouseDown:) {
                let _: () = objc2::msg_send![child, rightMouseDown: &*event];
            } else if selector == objc2::sel!(rightMouseUp:) {
                let _: () = objc2::msg_send![child, rightMouseUp: &*event];
            } else if selector == objc2::sel!(otherMouseDown:) {
                let _: () = objc2::msg_send![child, otherMouseDown: &*event];
            } else {
                let _: () = objc2::msg_send![child, otherMouseUp: &*event];
            }
        }
        Ok(json!({}))
    }

    fn key(view: &WebView, params: &Value) -> Result<Value, String> {
        let name = params["key"].as_str().ok_or("缺少网页按键")?;
        let (key_code, character) = match name {
            "Enter" => (36, '\r'),
            "Tab" => (48, '\t'),
            "Backspace" => (51, '\u{7f}'),
            "Escape" => (53, '\u{1b}'),
            "Delete" => (117, '\u{f728}'),
            "ArrowLeft" => (123, '\u{f702}'),
            "ArrowRight" => (124, '\u{f703}'),
            "ArrowDown" => (125, '\u{f701}'),
            "ArrowUp" => (126, '\u{f700}'),
            "Home" => (115, '\u{f729}'),
            "End" => (119, '\u{f72b}'),
            "PageUp" => (116, '\u{f72c}'),
            "PageDown" => (121, '\u{f72d}'),
            " " => (49, ' '),
            _ if name.chars().count() == 1 => {
                let ch = name.chars().next().unwrap();
                let codes = "asdfhgzxcv bqwerty123465=97-80]ou[ip\nlj'k;\\,/nm.";
                let code = codes
                    .chars()
                    .position(|known| known.eq_ignore_ascii_case(&ch))
                    .unwrap_or(0) as u16;
                (code, ch)
            }
            _ => return Err("当前内嵌浏览器不支持该按键".into()),
        };
        let child = view.webview();
        let window = view.ns_window();
        let key_up = params["type"] == "keyUp";
        let text = NSString::from_str(&character.to_string());
        unsafe {
            let window_number: isize = objc2::msg_send![&*window, windowNumber];
            let event: Option<Retained<AnyObject>> = objc2::msg_send![class!(NSEvent),
                keyEventWithType: if key_up { 11usize } else { 10usize },
                location: NSPoint::ZERO, modifierFlags: modifier_flags(params["modifiers"].as_u64().unwrap_or_default()),
                timestamp: event_timestamp(), windowNumber: window_number, context: std::ptr::null::<AnyObject>(),
                characters: &*text, charactersIgnoringModifiers: &*text, isARepeat: false, keyCode: key_code
            ];
            let event = event.ok_or("无法创建网页键盘事件")?;
            if key_up {
                let _: () = objc2::msg_send![&*child, keyUp: &*event];
            } else {
                let _: () = objc2::msg_send![&*child, keyDown: &*event];
            }
        }
        Ok(json!({}))
    }

    fn event_timestamp() -> f64 {
        let info: Retained<AnyObject> =
            unsafe { objc2::msg_send![class!(NSProcessInfo), processInfo] };
        unsafe { objc2::msg_send![&*info, systemUptime] }
    }

    fn modifier_flags(cdp: u64) -> usize {
        let mut flags = 0;
        for (bit, flag) in [(1, 1 << 19), (2, 1 << 18), (4, 1 << 20), (8, 1 << 17)] {
            if cdp & bit != 0 {
                flags |= flag;
            }
        }
        flags
    }

    fn capture(view: &WebView, reply: Reply) -> Result<(), String> {
        let reply = RefCell::new(Some(reply));
        let completion = RcBlock::new(move |image: *mut AnyObject, error: *mut NSError| {
            let result = unsafe {
                if let Some(error) = error.as_ref() {
                    Err(error.localizedDescription().to_string())
                } else {
                    encode_image(image)
                }
            };
            if let Some(reply) = reply.borrow_mut().take() {
                let _ = reply.send(result);
            }
        });
        unsafe {
            let _: () = objc2::msg_send![&*view.webview(), takeSnapshotWithConfiguration: std::ptr::null::<AnyObject>(), completionHandler: &*completion];
        }
        Ok(())
    }

    unsafe fn encode_image(image: *mut AnyObject) -> Result<Value, String> {
        let image = unsafe { image.as_ref() }.ok_or("网页没有返回图像")?;
        let tiff: Option<Retained<NSData>> = unsafe { objc2::msg_send![image, TIFFRepresentation] };
        let tiff = tiff.ok_or("网页图像为空")?;
        let bitmap: Option<Retained<AnyObject>> =
            unsafe { objc2::msg_send![class!(NSBitmapImageRep), imageRepWithData: &*tiff] };
        let bitmap = bitmap.ok_or("无法转换网页图像")?;
        let properties: Retained<AnyObject> =
            unsafe { objc2::msg_send![class!(NSDictionary), dictionary] };
        let png: Option<Retained<NSData>> = unsafe {
            objc2::msg_send![&*bitmap, representationUsingType: 4usize, properties: &*properties]
        };
        let png = png.ok_or("无法编码网页截图")?;
        Ok(json!({"data":STANDARD.encode(png.to_vec())}))
    }
}
