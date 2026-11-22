//! Preserve the browser's own popup relationship and observe its close event.
use wry::WebView;

#[cfg(target_os = "macos")]
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    DefinedClass, MainThreadMarker, MainThreadOnly,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSObject, NSObjectProtocol};
#[cfg(target_os = "macos")]
use objc2_web_kit::{WKUIDelegate, WKWebView};

#[cfg(target_os = "macos")]
pub(crate) struct CloseState {
    original: Retained<AnyObject>,
    close: Box<dyn Fn()>,
}

#[cfg(target_os = "macos")]
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EchoAgentWebKitPopupDelegate"]
    #[ivars = CloseState]
    pub(crate) struct CloseHandler;

    impl CloseHandler {
        #[unsafe(method(respondsToSelector:))]
        fn responds(&self, selector: Sel) -> bool {
            unsafe {
                let own: bool = msg_send![super(self), respondsToSelector: selector];
                own || msg_send![&*self.ivars().original, respondsToSelector: selector]
            }
        }

        #[unsafe(method_id(forwardingTargetForSelector:))]
        fn forward(&self, selector: Sel) -> Option<Retained<AnyObject>> {
            let responds: bool = unsafe { msg_send![&*self.ivars().original, respondsToSelector: selector] };
            responds.then(|| self.ivars().original.clone())
        }
    }

    unsafe impl NSObjectProtocol for CloseHandler {}

    unsafe impl WKUIDelegate for CloseHandler {
        #[unsafe(method(webViewDidClose:))]
        unsafe fn did_close(&self, _: &WKWebView) {
            (self.ivars().close)();
        }
    }
);

#[cfg(target_os = "macos")]
pub(super) fn watch_close(
    view: &WebView,
    close: impl Fn() + 'static,
) -> Result<Retained<CloseHandler>, String> {
    use wry::WebViewExtMacOS;
    let mtm = MainThreadMarker::new().ok_or("网页弹窗必须在主线程创建")?;
    let webview = view.webview();
    let original: Option<Retained<AnyObject>> = unsafe { msg_send![&*webview, UIDelegate] };
    let original = original.ok_or("无法识别网页弹窗的窗口委托")?;
    let handler = CloseHandler::alloc(mtm).set_ivars(CloseState {
        original,
        close: Box::new(close),
    });
    let handler: Retained<CloseHandler> = unsafe { msg_send![super(handler), init] };
    unsafe { webview.setUIDelegate(Some(ProtocolObject::from_ref(&*handler))) };
    Ok(handler)
}

#[cfg(windows)]
pub(super) fn watch_close(view: &WebView, close: impl Fn() + 'static) -> Result<(), String> {
    use webview2_com::WindowCloseRequestedEventHandler;
    use wry::WebViewExtWindows;
    let handler = WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
        close();
        Ok(())
    }));
    unsafe { view.webview().add_WindowCloseRequested(&handler, &mut 0) }
        .map_err(|error| format!("无法观察网页弹窗关闭事件：{error}"))
}
