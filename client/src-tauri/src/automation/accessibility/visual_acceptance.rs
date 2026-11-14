use super::super::computer::ComputerController;
use super::*;

fn wait_file(directory: &std::path::Path, name: &str) -> String {
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(text) = std::fs::read_to_string(directory.join(name)) {
            return text;
        }
        assert!(Instant::now() < until, "missing fixture receipt: {name}");
        std::thread::sleep(Duration::from_millis(60));
    }
}
fn capture(
    c: &mut Context<Platform>,
    computer: &mut ComputerController,
    window: &str,
    token: &CancellationToken,
) -> super::super::computer::ScreenshotFrame {
    let output = c
        .run("_capture_foreground_frame", &json!({"ref":window}), token)
        .unwrap();
    computer
        .window_frame(
            output["capture"]["data"].as_str().unwrap(),
            serde_json::from_value(output["capture"]["windowBounds"].clone()).unwrap(),
            "Echo opaque canvas",
        )
        .unwrap()
}
fn click(
    c: &mut Context<Platform>,
    computer: &mut ComputerController,
    window: &str,
    token: &CancellationToken,
    x: f64,
    y_from_bottom: f64,
) {
    let frame = capture(c, computer, window, token);
    let meta = &frame.meta;
    let x = x * f64::from(meta.image_width) / meta.logical_width;
    let y =
        (meta.logical_height - y_from_bottom) * f64::from(meta.image_height) / meta.logical_height;
    let target=c.run("_guard_point",&json!({"windowRef":window,"x":meta.origin_x+x*meta.logical_width/f64::from(meta.image_width),"y":meta.origin_y+y*meta.logical_height/f64::from(meta.image_height)}),token).unwrap();
    let fresh = c
        .run("_capture_foreground_frame", &json!({"ref":window}), token)
        .unwrap();
    computer
        .verify_window_frame(
            &meta.frame_id,
            fresh["capture"]["data"].as_str().unwrap(),
            serde_json::from_value(fresh["capture"]["windowBounds"].clone()).unwrap(),
            Some((x, y)),
        )
        .unwrap();
    let (dx, dy) = computer
        .validate_visual_point(&meta.frame_id, x, y)
        .unwrap();
    let result = c
        .run(
            "_click_point",
            &json!({"ref":target["ref"],"x":dx,"y":dy,"button":"left","clickCount":1}),
            token,
        )
        .unwrap();
    assert_eq!(result["clicked"], true, "{result}");
}

#[test]
#[ignore = "Real opaque GUI input, only runner-owned temporary apps; use test-desktop-visual.mjs"]
fn actual_opaque_canvas_flow() {
    let directory = std::fs::canonicalize(
        std::env::var("ECHO_NATIVE_VISUAL_FIXTURES").expect("Use scripts/test-desktop-visual.mjs"),
    )
    .unwrap();
    assert!(directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("echo-visual-native-"));
    let original = macos::foreground_pid().unwrap();
    struct Restore {
        original: Option<i32>,
        owned: Vec<i32>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            if macos::foreground_pid()
                .ok()
                .flatten()
                .is_some_and(|pid| self.owned.contains(&pid))
            {
                if let Some(pid) = self.original {
                    let _ = macos_restore_foreground(pid);
                }
            }
        }
    }
    let mut restore = Restore {
        original,
        owned: vec![],
    };
    let launch = |role: &str, overlay_only: bool| {
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg("-n");
        if overlay_only {
            command.arg("-g");
        }
        command
            .arg("-a")
            .arg(directory.join(format!("{role}.app")))
            .arg("--args")
            .arg(&directory)
            .arg(role);
        if overlay_only {
            command.arg("--overlay-only");
        }
        if std::env::var("ECHO_NATIVE_SHARING_NONE_FIXTURE").as_deref() == Ok("1")
            && role == "canvas"
        {
            command.arg("--sharing-none");
        }
        assert!(command.status().unwrap().success());
        wait_file(&directory, &format!("{role}-pid"))
            .parse::<i32>()
            .unwrap()
    };
    let pid = launch("canvas", false);
    restore.owned.push(pid);
    let token = CancellationToken::new();
    let mut c = Context {
        backend: Platform::new().unwrap(),
        refs: HashMap::new(),
        approvals: HashMap::new(),
        pages: HashMap::new(),
    };
    let identity = directory
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replace('-', "")
        .to_lowercase();
    let state = c
        .run(
            "computer_context",
            &json!({"app":format!("com.echoagent.fixture.canvas.canvas.{identity}")}),
            &token,
        )
        .unwrap();
    let window = state["windowRef"]
        .as_str()
        .expect("opaque window")
        .to_owned();
    assert_eq!(
        state["interaction"]["visualFallbackRecommended"], true,
        "{state}"
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while macos::foreground_pid().unwrap() != Some(pid) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    }
    let input = c
        .run("_guard_input", &json!({"ref":window}), &token)
        .unwrap();
    assert_eq!(
        input["focusAvailable"], false,
        "fixture must reproduce unavailable AX keyboard focus"
    );
    if std::env::var("ECHO_NATIVE_SHARING_NONE_FIXTURE").as_deref() == Ok("1") {
        let image = c
            .run("_capture_foreground_frame", &json!({"ref":window}), &token)
            .unwrap();
        assert_eq!(
            image["capture"]["captureMode"],
            "avfoundation_foreground_window_region"
        );
    }
    let proof = c.run("_prepare", &json!({"ref":window}), &token).unwrap();
    let key = c
        .run(
            "computer_key_window",
            &json!({"ref":window,"_proof":proof["proof"],"shortcut":"cmd+f"}),
            &token,
        )
        .unwrap();
    assert_eq!(
        key["keyDispatched"], true,
        "menu command must work without AX focus: {key}"
    );
    assert_eq!(wait_file(&directory, "canvas-find"), "READY");
    let mut computer = ComputerController::default();
    click(&mut c, &mut computer, &window, &token, 224., 167.5);
    assert_eq!(wait_file(&directory, "canvas-click"), "SEARCH");
    let frame = capture(&mut c, &mut computer, &window, &token);
    let current = c
        .run("_capture_foreground_frame", &json!({"ref":window}), &token)
        .unwrap();
    computer
        .verify_window_frame(
            &frame.meta.frame_id,
            current["capture"]["data"].as_str().unwrap(),
            serde_json::from_value(current["capture"]["windowBounds"].clone()).unwrap(),
            None,
        )
        .unwrap();
    let body = "ECHO OPAQUE GUI 中文输入 🧪";
    computer.bind_input_token(token.clone());
    computer.type_text(&frame.meta.frame_id, body).unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if wait_file(&directory, "canvas-text") == body {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    }
    click(&mut c, &mut computer, &window, &token, 94., 76.);
    assert_eq!(wait_file(&directory, "canvas-sent"), body);
    let visual_check = |text: &str, y: f64| json!({"ref":window,"checks":[{"text":text,"region":{"x":0.05,"y":y,"width":0.9,"height":0.3}}]});
    let verified = c
        .run(
            "_verify_visual",
            &visual_check("ECHO OPAQUE GUI 中文输入", 0.2),
            &token,
        )
        .unwrap();
    assert_eq!(
        verified["verified"], true,
        "real OS OCR must read the fixture text: {verified}"
    );
    let wrong = c
        .run(
            "_verify_visual",
            &visual_check("ECHO OPAQUE GUI 中文输入 错误文字", 0.2),
            &token,
        )
        .unwrap();
    assert_eq!(wrong["verified"], false);
    let wrong_region = c
        .run(
            "_verify_visual",
            &visual_check("ECHO OPAQUE GUI 中文输入", 0.65),
            &token,
        )
        .unwrap();
    assert_eq!(
        wrong_region["verified"], false,
        "text outside the requested region must not be proof"
    );
    let scroll_frame = capture(&mut c, &mut computer, &window, &token);
    let fresh = c
        .run("_capture_foreground_frame", &json!({"ref":window}), &token)
        .unwrap();
    let point = (
        f64::from(scroll_frame.meta.image_width) / 2.,
        f64::from(scroll_frame.meta.image_height) / 2.,
    );
    c.run("_guard_point", &json!({"windowRef":window,"x":scroll_frame.meta.origin_x+scroll_frame.meta.logical_width/2.,"y":scroll_frame.meta.origin_y+scroll_frame.meta.logical_height/2.}), &token).unwrap();
    computer
        .verify_window_frame(
            &scroll_frame.meta.frame_id,
            fresh["capture"]["data"].as_str().unwrap(),
            serde_json::from_value(fresh["capture"]["windowBounds"].clone()).unwrap(),
            Some(point),
        )
        .unwrap();
    computer
        .scroll_at(&scroll_frame.meta.frame_id, 0, -80, Some(point))
        .unwrap();
    assert_eq!(wait_file(&directory, "canvas-scroll"), "SCROLLED");
    let frame = capture(&mut c, &mut computer, &window, &token);
    std::fs::write(
        directory.join("canvas.png"),
        base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &frame.png_base64,
        )
        .unwrap(),
    )
    .unwrap();
    let video = std::env::var("ECHO_NATIVE_SHARING_NONE_FIXTURE").as_deref() == Ok("1")
        || std::env::var("ECHO_NATIVE_FORCE_WINDOW_VIDEO").as_deref() == Ok("1");
    if video {
        std::fs::write(directory.join("canvas-overlay-command"), "show").unwrap();
        assert_eq!(wait_file(&directory, "canvas-overlay-shown"), "READY");
        assert_eq!(macos::foreground_pid().unwrap(), Some(pid));
        c.run("_capture_foreground_frame", &json!({"ref":window}), &token)
            .expect("native same-app floating UI must be included");
        std::fs::write(directory.join("canvas-overlay-command"), "hide").unwrap();
        assert_eq!(wait_file(&directory, "canvas-overlay-hidden"), "READY");
        capture(&mut c, &mut computer, &window, &token);
        std::fs::remove_file(directory.join("canvas-overlay-hidden")).unwrap();
        std::fs::write(directory.join("canvas-overlay-command"), "document").unwrap();
        assert_eq!(wait_file(&directory, "canvas-document-shown"), "READY");
        let error = c
            .run("_capture_foreground_frame", &json!({"ref":window}), &token)
            .unwrap_err();
        assert!(
            error.starts_with("target_changed:"),
            "independent same-PID standard windows must not become transients: {error}"
        );
        std::fs::write(directory.join("canvas-overlay-command"), "hide").unwrap();
        assert_eq!(wait_file(&directory, "canvas-overlay-hidden"), "READY");
    }
    // Overlay checks refresh the controller's sole input frame. Use that fresh
    // frame when proving a subsequent foreign-focus rejection.
    let frame = capture(&mut c, &mut computer, &window, &token);
    let mut other = launch("other", video);
    restore.owned.push(other);
    if video {
        assert_eq!(macos::foreground_pid().unwrap(), Some(pid));
        let error = c
            .run("_capture_foreground_frame", &json!({"ref":window}), &token)
            .unwrap_err();
        assert!(
            error.starts_with("target_changed:"),
            "foreign nonactivating overlay must be rejected: {error}"
        );
        std::fs::write(directory.join("other-overlay-command"), "hide").unwrap();
        assert_eq!(wait_file(&directory, "other-overlay-hidden"), "READY");
        std::fs::write(directory.join("other-overlay-command"),"quit").unwrap();
        assert_eq!(wait_file(&directory,"other-quit"),"READY");
        std::fs::remove_file(directory.join("other-pid")).unwrap();
        other=launch("other",false);
        restore.owned.push(other);
    }
    let deadline = Instant::now() + Duration::from_secs(4);
    while macos::foreground_pid().unwrap() != Some(other) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(c
        .run("_capture_foreground_frame", &json!({"ref":window}), &token)
        .unwrap_err()
        .starts_with("foreground_required"));
    assert!(computer
        .validate_visual_point(&frame.meta.frame_id, 10., 10.)
        .unwrap_err()
        .starts_with("focus_changed"));
    assert!(!directory.join("other-text").exists());
    std::fs::write(directory.join("evidence.json"),json!({"opaqueAX":true,"missingKeyboardFocus":true,"nativeMenuWithoutFocus":true,"windowBoundFrame":true,"actualPointerHit":true,"unicodeText":true,"fixtureSendReadback":true,"targetedScrollReadback":true,"foreignWindowRejected":true,"foreignTextUnchanged":true,"foregroundVideo":video,"visibleOverlayRejected":video}).to_string()).unwrap();
    println!("Opaque GUI full flow passed: {}", directory.display());
}

#[test]
#[ignore = "Explicit non-sending WeChat visual preflight; requires the user's unlocked local WeChat"]
fn actual_wechat_visual_preflight() {
    observe_wechat(false);
}

#[test]
#[ignore = "Inspect only the already-open WeChat Settings General page; explicit local authorization required"]
fn actual_wechat_settings_navigation() {
    assert_eq!(
        std::env::var("ECHO_NATIVE_WECHAT_SETTINGS").as_deref(),
        Ok("1")
    );
    observe_wechat(true);
}

fn observe_wechat(inspect_settings: bool) {
    assert_eq!(
        std::env::var("ECHO_NATIVE_WECHAT_PREFLIGHT").as_deref(),
        Ok("1")
    );
    let installed =
        super::super::targets::resolve("微信").expect("localized installed application alias");
    assert_eq!(installed.path, "/Applications/WeChat.app");
    assert!(
        super::super::policy::target_names(&installed.approval_details())
            .iter()
            .any(|name| super::super::targets::mentioned("请打开微信发送消息", name))
    );
    let token = CancellationToken::new();
    let mut c = Context {
        backend: Platform::new().unwrap(),
        refs: HashMap::new(),
        approvals: HashMap::new(),
        pages: HashMap::new(),
    };
    let original = macos::foreground_pid().unwrap();
    let owned_pid = c
        .backend
        .apps()
        .unwrap()
        .items
        .into_iter()
        .find(|(_, n)| n.identifier == "com.tencent.xinWeChat")
        .and_then(|(e, _)| c.backend.process_id(&e))
        .expect("WeChat must be running") as i32;
    struct AppRestore(Option<i32>, i32);
    impl Drop for AppRestore {
        fn drop(&mut self) {
            if macos::foreground_pid().ok().flatten() == Some(self.1) {
                if let Some(pid) = self.0 {
                    let _ = macos_restore_foreground(pid);
                }
            }
        }
    }
    let _app_restore = AppRestore(original, owned_pid);
    let path = std::fs::canonicalize("/Applications/WeChat.app").unwrap();
    let inspect_transient = std::env::var("ECHO_NATIVE_WECHAT_TRANSIENT").as_deref() == Ok("1");
    if !inspect_settings && !inspect_transient {
        assert!(c.backend.reopen_app(path.to_str().unwrap()).unwrap());
    }
    std::thread::sleep(Duration::from_millis(300));
    let state = c
        .run("computer_context", &json!({"app":"微信"}), &token)
        .unwrap();
    let target = state["windowRef"]
        .as_str()
        .expect("WeChat main window must be open")
        .to_owned();
    assert_eq!(
        state["identifier"], "com.tencent.xinWeChat",
        "the localized request must resolve the main WeChat application: {state}"
    );
    if let Ok(directory) = std::env::var("ECHO_NATIVE_WECHAT_OBSERVATION_DIR") {
        let directory = std::fs::canonicalize(directory).unwrap();
        assert_eq!(
            directory.parent(),
            Some(std::path::Path::new("/private/tmp"))
        );
        assert!(directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("echo-wechat-live-"));
        std::fs::write(
            directory.join("snapshot.json"),
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
    }
    let r = c.resolve(&target).unwrap();
    let pid = c.backend.process_id(&r.element).unwrap() as i32;
    let original = macos::foreground_pid().unwrap();
    struct Restore(Option<i32>, i32);
    impl Drop for Restore {
        fn drop(&mut self) {
            if macos::foreground_pid().ok().flatten() == Some(self.1) {
                if let Some(pid) = self.0 {
                    let _ = macos_restore_foreground(pid);
                }
            }
        }
    }
    let _restore = Restore(original, pid);
    let proof = c.run("_prepare", &json!({"ref":target}), &token).unwrap();
    let active = c
        .run(
            "computer_activate_window",
            &json!({"ref":target,"_proof":proof["proof"]}),
            &token,
        )
        .unwrap();
    assert_eq!(active["verified"], true, "{active}");
    let input = c
        .run("_guard_input", &json!({"ref":target}), &token)
        .unwrap();
    let frame = c
        .run("_capture_foreground_frame", &json!({"ref":target}), &token)
        .unwrap();
    assert_eq!(
        frame["guard"]["applicationIdentifier"],
        "com.tencent.xinWeChat"
    );
    let mut computer = ComputerController::default();
    let image = &frame["capture"];
    let bounds = serde_json::from_value(image["windowBounds"].clone()).unwrap();
    if inspect_transient {
        let target_ref = c.resolve(&target).unwrap();
        let app = c
            .backend
            .application_element(&target_ref.element)
            .unwrap()
            .unwrap();
        let listing = c.backend.windows(Some(&app)).unwrap();
        let (_, node) = listing
            .items
            .into_iter()
            .find(|(_, n)| n.name.is_empty() && n.bounds.is_some_and(|b| b[2] > 50. && b[2] < 500.))
            .expect("observed WeChat search popup");
        let popup = node.bounds.unwrap();
        c.run(
            "_guard_point",
            &json!({"windowRef":target,"x":popup[0]+popup[2]/2.,"y":popup[1]+popup[3]/2.}),
            &token,
        )
        .expect("native transient point must be trusted without sending input");
    }
    let shot = computer
        .window_frame(image["data"].as_str().unwrap(), bounds, "微信")
        .unwrap();
    let fresh = c
        .run("_capture_foreground_frame", &json!({"ref":target}), &token)
        .unwrap();
    computer
        .verify_window_frame(
            &shot.meta.frame_id,
            fresh["capture"]["data"].as_str().unwrap(),
            bounds,
            Some((20., 20.)),
        )
        .unwrap();
    let point = computer
        .validate_visual_point(&shot.meta.frame_id, 20., 20.)
        .unwrap();
    assert!(point.0.is_finite() && point.1.is_finite());
    if let Ok(directory) = std::env::var("ECHO_NATIVE_WECHAT_OBSERVATION_DIR") {
        let directory = std::fs::canonicalize(directory).unwrap();
        assert_eq!(
            directory.parent(),
            Some(std::path::Path::new("/private/tmp"))
        );
        assert!(directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("echo-wechat-live-"));
        std::fs::write(
            directory.join("window.png"),
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &shot.png_base64)
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            directory.join("snapshot.json"),
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        if inspect_settings {
            assert_eq!(state["windows"][0]["name"], "设置");
            let page = std::env::var("ECHO_NATIVE_WECHAT_SETTINGS_PAGE")
                .unwrap_or_else(|_| "general".into());
            assert!(["general", "plugins"].contains(&page.as_str()));
            let top = if page == "plugins" { 220. } else { 100. };
            click(
                &mut c,
                &mut computer,
                &target,
                &token,
                60.,
                shot.meta.logical_height - top,
            );
            std::thread::sleep(Duration::from_millis(300));
            if std::env::var("ECHO_NATIVE_WECHAT_SETTINGS_SCROLL").as_deref() == Ok("1") {
                let f = capture(&mut c, &mut computer, &target, &token);
                let fresh = c
                    .run("_capture_foreground_frame", &json!({"ref":target}), &token)
                    .unwrap();
                let point = (
                    f64::from(f.meta.image_width) * 0.8,
                    f64::from(f.meta.image_height) * 0.7,
                );
                c.run("_guard_point",&json!({"windowRef":target,"x":f.meta.origin_x+f.meta.logical_width*0.8,"y":f.meta.origin_y+f.meta.logical_height*0.7}),&token).unwrap();
                computer
                    .verify_window_frame(
                        &f.meta.frame_id,
                        fresh["capture"]["data"].as_str().unwrap(),
                        serde_json::from_value(fresh["capture"]["windowBounds"].clone()).unwrap(),
                        Some(point),
                    )
                    .unwrap();
                computer.bind_input_token(token.clone());
                computer
                    .scroll_at(&f.meta.frame_id, 0, 550, Some(point))
                    .unwrap();
                std::thread::sleep(Duration::from_millis(300));
            }
            let after = capture(&mut c, &mut computer, &target, &token);
            std::fs::write(
                directory.join(format!("{page}.png")),
                base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &after.png_base64,
                )
                .unwrap(),
            )
            .unwrap();
        }
        let ocr=c.run("_verify_visual",&json!({"ref":target,"checks":[{"text":"__ECHO_NEGATIVE_OCR_CHECK__","region":{"x":0,"y":0,"width":1,"height":1}}]}),&token).unwrap();
        assert_eq!(
            ocr["verified"], false,
            "unobserved text must not produce proof"
        );
        std::fs::write(
            directory.join("ocr.json"),
            serde_json::to_vec_pretty(&ocr).unwrap(),
        )
        .unwrap();
        if std::env::var("ECHO_NATIVE_WECHAT_SENT_PROOF").as_deref()==Ok("1") {
            let proof=c.run("_verify_visual",&json!({"ref":target,"checks":[
                {"text":"文件传输助手","region":{"x":0.35,"y":0.012,"width":0.13,"height":0.045}},
                {"text":"EchoAgent 微信端到端测试，请忽略。","region":{"x":0.648,"y":0.687,"width":0.28,"height":0.05}},
                {"text":"长按鼠标 语音输入","region":{"x":0.365,"y":0.773,"width":0.15,"height":0.035}}
            ]}),&token).unwrap();
            assert_eq!(proof["verified"],true,"read-only sent-history proof: {proof}");
            std::fs::write(directory.join("sent-proof.json"),serde_json::to_vec_pretty(&proof).unwrap()).unwrap();
        }
        println!("Window observation saved in {}", directory.display());
    }
    println!("WeChat window observation passed: settingsNavigation={inspect_settings}, keyboardFocusAvailable={}, nativeTextInputs={}, frame={}x{}, no message sent",input["focusAvailable"],state["interaction"]["nativeTextInputs"],shot.meta.image_width,shot.meta.image_height);
}
