//! Deliberately isolated, opt-in native Echo Code acceptance.
//! There is no command accepting caller supplied JavaScript or path grants.
use std::sync::atomic::{AtomicBool, Ordering};

static RUN_STARTED: AtomicBool = AtomicBool::new(false);
static WORKSPACE_FIXTURE_PREPARED: AtomicBool = AtomicBool::new(false);
static POINTER_NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static POINTER_SENT: AtomicBool = AtomicBool::new(false);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InlinePointerRequest {
    nonce: String,
    path: String,
    frame: ReviewRect,
    input: ReviewRect,
    viewport: ReviewSize,
    child_viewport: ReviewSize,
}

#[derive(serde::Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct ReviewRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(serde::Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct ReviewSize {
    width: f64,
    height: f64,
}

fn pointer_target(request: &InlinePointerRequest) -> Result<(f64, f64), String> {
    let frame = request.frame;
    let input = request.input;
    let viewport = request.viewport;
    let child = request.child_viewport;
    let valid_size = |size: ReviewSize| {
        size.width.is_finite()
            && size.height.is_finite()
            && (200.0..=4096.0).contains(&size.width)
            && (200.0..=4096.0).contains(&size.height)
    };
    let inside = |rect: ReviewRect, size: ReviewSize| {
        [rect.x, rect.y, rect.width, rect.height]
            .iter()
            .all(|value| value.is_finite())
            && rect.x >= 0.0
            && rect.y >= 0.0
            && rect.width > 0.0
            && rect.height > 0.0
            && rect.x + rect.width <= size.width + 1.0
            && rect.y + rect.height <= size.height + 1.0
    };
    if !valid_size(viewport)
        || !valid_size(child)
        || !inside(frame, viewport)
        || !inside(input, child)
        || (child.width - frame.width).abs() > 2.0
        || (child.height - frame.height).abs() > 2.0
        || input.width > 64.0
        || input.height > 100.0
    {
        return Err(
            "Native inline pointer target is outside its own visible frame/input bounds".into(),
        );
    }
    Ok((
        frame.x + input.x + input.width / 2.0,
        frame.y + input.y + input.height / 2.0,
    ))
}

fn fixed_workspace_fixture(home: &std::path::Path) -> Result<Vec<String>, String> {
    [
        ("default", "echo-native-workspace"),
        ("other", "echo-isolated-other"),
    ]
    .into_iter()
    .map(|(name, package)| {
        let expected = home.join("workspaces").join(name);
        let canonical = std::fs::canonicalize(&expected).map_err(|error| error.to_string())?;
        if canonical != expected || !canonical.is_dir() || !canonical.starts_with(home) {
            return Err(
                "Native workspace fixture must use ordinary directories inside the isolated home"
                    .into(),
            );
        }
        let manifest = canonical.join("package.json");
        if std::fs::symlink_metadata(&manifest)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
            || std::fs::metadata(&manifest)
                .map_err(|error| error.to_string())?
                .len()
                > 8192
        {
            return Err("Invalid native workspace fixture manifest".into());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if value.get("name").and_then(serde_json::Value::as_str) != Some(package) {
            return Err(
                "Native workspace fixture manifest does not match its fixed project".into(),
            );
        }
        Ok(canonical.to_string_lossy().into_owned())
    })
    .collect()
}

fn require_isolated_code_review(
    window: &tauri::WebviewWindow,
) -> Result<std::path::PathBuf, String> {
    if std::env::var("ECHO_DESKTOP_REVIEW").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_CODE").as_deref() != Ok("1")
        || window.label() != "main"
    {
        return Err("Code acceptance requires the dedicated review flags and main window".into());
    }
    let selected = crate::paths::redirected_data_home()
        .ok_or("Code acceptance requires an explicitly redirected data home")?;
    let configured = std::env::var_os("ECHO_AGENT_HOME")
        .map(std::path::PathBuf::from)
        .ok_or("Code acceptance requires ECHO_AGENT_HOME")?;
    let canonical = std::fs::canonicalize(&selected).map_err(|error| error.to_string())?;
    if canonical != selected || canonical != configured || !canonical.is_dir() {
        return Err("Code acceptance requires the canonical, existing redirected home".into());
    }
    let temporary =
        std::fs::canonicalize(std::env::temp_dir()).map_err(|error| error.to_string())?;
    let private_tmp = std::fs::canonicalize("/private/tmp").ok();
    let inside_temporary_home = (canonical.starts_with(&temporary) && canonical != temporary)
        || private_tmp
            .as_ref()
            .is_some_and(|root| canonical.starts_with(root) && canonical != *root);
    if !inside_temporary_home {
        return Err("Code acceptance may only use a nested temporary data home".into());
    }
    let name = canonical
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or("Code acceptance home must have a readable fixture name")?;
    if !name.starts_with("echo-native-review-") {
        return Err("Code acceptance must continue an isolated native review fixture".into());
    }
    Ok(canonical)
}

/// Read only the current fixture's own Cocoa window and WKWebView. No caller
/// may select a process, window or script, and no native object leaves the
/// main-thread callback. Dispatching focus is not proof of keyboard focus.
pub(crate) async fn inline_focus_diagnostics(
    window: &tauri::WebviewWindow,
) -> Result<serde_json::Value, String> {
    let home = require_isolated_code_review(window)?;
    if std::env::var("ECHO_DESKTOP_REVIEW_CODING_WORKSPACE").as_deref() != Ok("1")
        || std::env::var("ECHO_DESKTOP_REVIEW_CODING_INLINE_ONLY").as_deref() != Ok("1")
    {
        return Err("Native focus diagnostics require the dedicated inline-only fixture".into());
    }
    fixed_workspace_fixture(&home)?;
    #[cfg(not(target_os = "macos"))]
    {
        Err("Native focus diagnostics require macOS".into())
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::{class, msg_send, runtime::AnyObject, MainThreadMarker};

        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .with_webview(move |webview| {
                let observed = (|| {
                    let _marker = MainThreadMarker::new()
                        .ok_or("Native focus diagnostics did not run on the main thread")?;
                    // Tauri documents these pointers as the WKWebView and its
                    // own NSWindow. Inspect only metadata on those objects.
                    unsafe {
                        let view = webview.inner().cast::<AnyObject>();
                        let native = webview.ns_window().cast::<AnyObject>();
                        let view_ref = view.as_ref().ok_or("WKWebView is unavailable")?;
                        let native_ref = native.as_ref().ok_or("NSWindow is unavailable")?;
                        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                        let app = app.as_ref().ok_or("NSApplication is unavailable")?;
                        let responder: *mut AnyObject = msg_send![native_ref, firstResponder];
                        let key_window: *mut AnyObject = msg_send![app, keyWindow];
                        let view_window: *mut AnyObject = msg_send![view_ref, window];
                        let app_active: bool = msg_send![app, isActive];
                        let is_key: bool = msg_send![native_ref, isKeyWindow];
                        let is_main: bool = msg_send![native_ref, isMainWindow];
                        let is_visible: bool = msg_send![native_ref, isVisible];
                        let view_hidden: bool = msg_send![view_ref, isHidden];
                        let accepts_responder: bool = msg_send![view_ref, acceptsFirstResponder];
                        Ok::<_, &'static str>(serde_json::json!({
                            "source": "main-thread observation of this fixture's own NSApplication/NSWindow/WKWebView",
                            "applicationActive": app_active,
                            "windowIsKey": is_key,
                            "windowIsMain": is_main,
                            "windowIsVisible": is_visible,
                            "applicationKeyWindowIsOwn": key_window == native,
                            "webviewWindowIsOwn": view_window == native,
                            "webviewClass": view_ref.class().name().to_string_lossy(),
                            "webviewIsHidden": view_hidden,
                            "webviewAcceptsFirstResponder": accepts_responder,
                            "firstResponderClass": responder.as_ref().map(|object| object.class().name().to_string_lossy().into_owned()),
                            "firstResponderIsWebview": responder == view,
                        }))
                    }
                })();
                let _ = sender.send(observed);
            })
            .map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .map_err(|_| "Native focus diagnostics timed out".to_string())?
            .map_err(|_| "Native focus diagnostics were interrupted".to_string())?
            .map_err(str::to_string)
    }
}

/// One real mouse gesture in this fixture's own WK view. The fixed renderer
/// supplies only its measured input/frame bounds, authenticated by the native
/// launch nonce. This never posts global input or targets another application.
pub(crate) async fn inline_pointer_focus(
    window: &tauri::WebviewWindow,
    request: InlinePointerRequest,
) -> Result<serde_json::Value, String> {
    let home = require_isolated_code_review(window)?;
    if std::env::var("ECHO_DESKTOP_REVIEW_CODING_WORKSPACE").as_deref() != Ok("1")
        || POINTER_NONCE.get() != Some(&request.nonce)
    {
        return Err("Native inline pointer requires the authenticated workspace fixture".into());
    }
    fixed_workspace_fixture(&home)?;
    let expected = home.join("workspaces/default/completion.cjs");
    if request.path != expected.to_string_lossy()
        || std::fs::canonicalize(&expected).map_err(|error| error.to_string())? != expected
        || !std::fs::symlink_metadata(&expected)
            .map_err(|error| error.to_string())?
            .is_file()
        || std::fs::read(&expected).map_err(|error| error.to_string())?
            != b"// User inline completion scratch file.\n"
    {
        return Err("Native inline pointer requires the unchanged fixed completion file".into());
    }
    let (css_x, css_y) = pointer_target(&request)?;
    if POINTER_SENT.swap(true, Ordering::AcqRel) {
        return Err("Native inline pointer gesture cannot be replayed".into());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (css_x, css_y);
        Err("Native inline pointer requires macOS".into())
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::{class, msg_send, runtime::AnyObject, MainThreadMarker};
        use objc2_foundation::{NSPoint, NSRect};
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window.with_webview(move |webview| {
            let result = (|| {
                let _marker = MainThreadMarker::new().ok_or("Native inline pointer requires the main thread")?;
                unsafe {
                    let view = webview.inner().cast::<AnyObject>();
                    let native = webview.ns_window().cast::<AnyObject>();
                    let view_ref = view.as_ref().ok_or("WK view unavailable")?;
                    let native_ref = native.as_ref().ok_or("Own NSWindow unavailable")?;
                    let owner: *mut AnyObject = msg_send![view_ref, window];
                    let key: bool = msg_send![native_ref, isKeyWindow];
                    let visible: bool = msg_send![native_ref, isVisible];
                    let hidden: bool = msg_send![view_ref, isHiddenOrHasHiddenAncestor];
                    let bounds: NSRect = msg_send![view_ref, bounds];
                    let visible_rect: NSRect = msg_send![view_ref, visibleRect];
                    let flipped: bool = msg_send![view_ref, isFlipped];
                    if owner != native || !key || !visible || hidden
                        || bounds.size.width <= 0.0 || bounds.size.height <= 0.0
                        || (bounds.size.width - request.viewport.width).abs() > 3.0
                        || (bounds.size.height - request.viewport.height).abs() > 3.0 {
                        return Err("Native pointer view/window ownership, visibility or viewport changed");
                    }
                    let local = NSPoint::new(bounds.origin.x + css_x,
                        bounds.origin.y + if flipped { css_y } else { bounds.size.height - css_y });
                    if local.x < visible_rect.origin.x || local.y < visible_rect.origin.y
                        || local.x >= visible_rect.origin.x + visible_rect.size.width
                        || local.y >= visible_rect.origin.y + visible_rect.size.height {
                        return Err("Native pointer target is outside the own visible WK view");
                    }
                    let nil_view: *mut AnyObject = std::ptr::null_mut();
                    let location: NSPoint = msg_send![view_ref, convertPoint: local, toView: nil_view];
                    let window_number: isize = msg_send![native_ref, windowNumber];
                    let process_info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
                    let process_info = process_info.as_ref().ok_or("Native process clock unavailable")?;
                    let time: f64 = msg_send![process_info, systemUptime];
                    let nil_context: *mut AnyObject = std::ptr::null_mut();
                    for (kind, pressure) in [(1usize, 1.0f32), (2usize, 0.0f32)] {
                        let event: *mut AnyObject = msg_send![class!(NSEvent),
                            mouseEventWithType: kind, location: location, modifierFlags: 0usize,
                            timestamp: time, windowNumber: window_number, context: nil_context,
                            eventNumber: 1isize, clickCount: 1isize, pressure: pressure];
                        let event = event.as_ref().ok_or("Native mouse event could not be created")?;
                        let actual_number: isize = msg_send![event, windowNumber];
                        if actual_number != window_number { return Err("Native event belongs to another window"); }
                        let _: () = msg_send![native_ref, sendEvent: event];
                    }
                    Ok::<_, &'static str>(serde_json::json!({
                        "source": "one own NSWindow.sendEvent mouse-down/up, NSView.convertPoint(toView:nil); no global input",
                        "windowNumber": window_number, "eventWindowNumber": window_number,
                        "viewIsFlipped": flipped, "viewBounds": {"x":bounds.origin.x,"y":bounds.origin.y,"width":bounds.size.width,"height":bounds.size.height},
                        "cssPoint": {"x":css_x,"y":css_y}, "windowPoint": {"x":location.x,"y":location.y},
                        "downAndUpDispatched": true
                    }))
                }
            })();
            let _ = sender.send(result);
        }).map_err(|error| error.to_string())?;
        tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .map_err(|_| "Native inline pointer timed out".to_string())?
            .map_err(|_| "Native inline pointer interrupted".to_string())?
            .map_err(str::to_string)
    }
}

/// The caller must also apply the existing `require_review_launch` gate before
/// dispatching its dedicated code branch. This function checks its own boundary.
pub(crate) async fn run(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    let home = require_isolated_code_review(&window)?;
    if RUN_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, home);
        Err("Native Code acceptance currently requires the macOS WKWebView driver".into())
    }
    #[cfg(target_os = "macos")]
    {
        if std::env::var("ECHO_DESKTOP_REVIEW_CODING_WORKSPACE").as_deref() == Ok("1")
            && !WORKSPACE_FIXTURE_PREPARED.swap(true, Ordering::AcqRel)
        {
            use tauri::Manager;
            let projects = fixed_workspace_fixture(&home)?;
            for project in &projects {
                app.state::<crate::shell_fs::FilesystemAccess>()
                    .authorize_workspace(project)?;
            }
            // Explicit fixture setup only: pre-existing recent-project metadata
            // and narrowly scoped native directory grants. No task, session,
            // model result or verification state is synthesized. Reload before
            // the journey so production hydration consumes the ordinary keys.
            let recent = serde_json::to_string(
                &projects
                    .iter()
                    .map(|cwd| serde_json::json!({"cwd":cwd,"sessionCount":0}))
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| error.to_string())?;
            let script=format!("localStorage.setItem('echo-coding-workspaces',{});localStorage.setItem('echo-coding-active-cwd',{});location.reload();",serde_json::to_string(&recent).map_err(|error|error.to_string())?,serde_json::to_string(&projects[0]).map_err(|error|error.to_string())?);
            RUN_STARTED.store(false, Ordering::Release);
            return window.eval(script).map_err(|error| error.to_string());
        }
        run_macos(app, window, home).await
    }
}

#[cfg(test)]
mod workspace_fixture_tests {
    use super::*;
    #[test]
    fn pointer_is_limited_to_measured_input_inside_the_own_frame() {
        let mut request = InlinePointerRequest {
            nonce: "test-only".into(),
            path: "/unused-fixture-path".into(),
            frame: ReviewRect {
                x: 1.0,
                y: 45.0,
                width: 860.0,
                height: 755.0,
            },
            input: ReviewRect {
                x: 230.0,
                y: 57.0,
                width: 1.0,
                height: 18.0,
            },
            viewport: ReviewSize {
                width: 1280.0,
                height: 800.0,
            },
            child_viewport: ReviewSize {
                width: 860.0,
                height: 755.0,
            },
        };
        assert_eq!(pointer_target(&request).unwrap(), (231.5, 111.0));
        request.input.x = -1.0;
        assert!(pointer_target(&request).is_err());
        request.input.x = 900.0;
        assert!(pointer_target(&request).is_err());
        request.input.x = 230.0;
        request.frame.y = 100.0;
        assert!(pointer_target(&request).is_err());
        request.frame.y = 45.0;
        request.input.width = 1000.0;
        assert!(pointer_target(&request).is_err());
        request.input.width = 1.0;
        request.child_viewport.width = 400.0;
        assert!(pointer_target(&request).is_err());
        request.child_viewport.width = f64::NAN;
        assert!(pointer_target(&request).is_err());
    }

    #[test]
    fn fixture_setup_rejects_missing_foreign_and_symlink_projects() {
        let directory = tempfile::tempdir().unwrap();
        assert!(fixed_workspace_fixture(directory.path()).is_err());
        for (name, package) in [
            ("default", "echo-native-workspace"),
            ("other", "foreign-project"),
        ] {
            let path = directory.path().join("workspaces").join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("package.json"),
                serde_json::json!({"name":package}).to_string(),
            )
            .unwrap();
        }
        assert!(fixed_workspace_fixture(directory.path()).is_err());
        std::fs::write(
            directory.path().join("workspaces/other/package.json"),
            r#"{"name":"echo-isolated-other"}"#,
        )
        .unwrap();
        let root = directory.path().canonicalize().unwrap();
        assert_eq!(fixed_workspace_fixture(&root).unwrap().len(), 2);
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let other = root.join("workspaces/other");
            std::fs::remove_dir_all(&other).unwrap();
            std::os::unix::fs::symlink(outside.path(), other).unwrap();
            assert!(fixed_workspace_fixture(&root).is_err());
        }
    }
}

#[cfg(target_os = "macos")]
async fn run_macos(
    _app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    home: std::path::PathBuf,
) -> Result<(), String> {
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_foundation::NSString;
    use objc2_web_kit::{WKUserContentController, WKUserScript, WKUserScriptInjectionTime};

    let home = home
        .to_str()
        .ok_or("Native Code acceptance requires a UTF-8 temporary fixture path")?;
    // Two independently random UUIDv7 tails, created natively per launch. The
    // nonce is not exposed in a child global, persisted, or printed in reports.
    let nonce = format!("{}{}", uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let workspace_journey =
        std::env::var("ECHO_DESKTOP_REVIEW_CODING_WORKSPACE").as_deref() == Ok("1");
    if workspace_journey {
        POINTER_NONCE
            .set(nonce.clone())
            .map_err(|_| "Native pointer nonce was already installed")?;
    }
    let scope = serde_json::json!({
        "home": home,
        "workspaceJourney": workspace_journey,
        "inlineOnly": workspace_journey
            && std::env::var("ECHO_DESKTOP_REVIEW_CODING_INLINE_ONLY").as_deref() == Ok("1"),
        "nonce": nonce,
        "continueExistingTask": std::env::var("ECHO_DESKTOP_REVIEW_CODE_CONTINUE").as_deref() == Ok("1"),
        "continueDeliveryReview": std::env::var("ECHO_DESKTOP_REVIEW_CODE_DELIVERY_CONTINUE").as_deref() == Ok("1"),
    });
    let child_script = format!(
        "{}({});",
        if workspace_journey {
            include_str!("../../scripts/fixtures/native-coding-workspace-frame.js")
        } else {
            include_str!("../../scripts/fixtures/native-coding-frame-review.js")
        },
        scope
    );
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |webview| {
            let installed = (|| {
                let marker = MainThreadMarker::new()
                    .ok_or("WK script installation did not run on the main thread")?;
                // Tauri documents this macOS controller as WKUserContentController.
                // The controller retains the script; preserve the existing Tauri
                // user scripts and their production IPC isolation.
                unsafe {
                    let controller = webview.controller().cast::<WKUserContentController>();
                    let controller = controller
                        .as_ref()
                        .ok_or("WK user content controller is unavailable")?;
                    let source = NSString::from_str(&child_script);
                    let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                        WKUserScript::alloc(marker),
                        &source,
                        WKUserScriptInjectionTime::AtDocumentStart,
                        false,
                    );
                    controller.addUserScript(&script);
                }
                Ok::<(), &'static str>(())
            })();
            let _ = sender.send(installed);
        })
        .map_err(|error| error.to_string())?;
    tokio::time::timeout(std::time::Duration::from_secs(15), receiver)
        .await
        .map_err(|_| "WK script installation timed out".to_string())?
        .map_err(|_| "WK script installation was interrupted".to_string())?
        .map_err(str::to_string)?;

    let launch = include_str!("../../scripts/fixtures/native-coding-launch.js");
    let parent_script = if workspace_journey {
        format!(
            "{}({});",
            include_str!("../../scripts/fixtures/native-coding-workspace-review.js"),
            scope
        )
    } else {
        format!(
            "{}\n{}({});",
            include_str!("../../scripts/fixtures/native-coding-review.js"),
            launch,
            scope
        )
    };
    // A bare acceptance executable can start behind the invoking application.
    // Real inline providers require real native text focus; programmatic
    // Monaco commands alone must not bypass that production focus gate.
    window.show().map_err(|error| error.to_string())?;
    window.unminimize().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;
    // WebviewWindow::set_focus delegates only to the native window. The
    // public Webview API additionally makes this WKWebView first responder.
    let native_webview: &tauri::Webview = window.as_ref();
    native_webview
        .set_focus()
        .map_err(|error| error.to_string())?;
    window
        .eval(&parent_script)
        .map_err(|error| error.to_string())
}
