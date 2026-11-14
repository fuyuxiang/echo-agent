//! OS-wide Ctrl+Alt+Escape. Callback only signals the normal cancellation path.
use std::sync::atomic::{AtomicBool, Ordering};
static SIGNAL: std::sync::OnceLock<fn(&'static str)> = std::sync::OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);
static AVAILABLE: AtomicBool = AtomicBool::new(false);
static MONITOR_AVAILABLE: AtomicBool = AtomicBool::new(false);
pub fn monitor_available() -> bool {
    MONITOR_AVAILABLE.load(Ordering::SeqCst)
}
pub fn available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}
pub fn start(callback: fn(&'static str)) {
    let _ = SIGNAL.set(callback);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        run();
        AVAILABLE.store(false, Ordering::SeqCst);
        MONITOR_AVAILABLE.store(false, Ordering::SeqCst);
        STARTED.store(false, Ordering::SeqCst);
    });
}
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn signal(reason: &'static str) {
    if let Some(callback) = SIGNAL.get() {
        callback(reason);
    }
}
#[cfg(target_os = "macos")]
fn run() {
    use core_graphics::event::*;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopRun();
        fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
        fn CFRunLoopStop(loop_ref: *mut std::ffi::c_void);
    }
    let _ = CGEventTap::with_enabled(
        CGEventTapLocation::AnnotatedSession,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![
            CGEventType::KeyDown,
            CGEventType::LeftMouseDown,
            CGEventType::RightMouseDown,
            CGEventType::OtherMouseDown,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
            CGEventType::ScrollWheel,
        ],
        |_, kind, event| {
            if matches!(
                kind,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            ) {
                MONITOR_AVAILABLE.store(false, Ordering::SeqCst);
                AVAILABLE.store(false, Ordering::SeqCst);
                super::desktop_coordinator::monitor_lost();
                unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
                return CallbackResult::Keep;
            }
            let flags = event.get_flags();
            let key_down = matches!(kind, CGEventType::KeyDown);
            let target = event.get_integer_value_field(EventField::EVENT_TARGET_UNIX_PROCESS_ID);
            let foreground_pid = || super::accessibility::macos_foreground_pid().ok().flatten();
            let host_input = target == i64::from(std::process::id())
                || (target <= 0 && foreground_pid() == Some(std::process::id() as i32));
            if key_down
                && event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) == 53
                && flags
                    .contains(CGEventFlags::CGEventFlagControl | CGEventFlags::CGEventFlagAlternate)
            {
                super::desktop_coordinator::interrupt_current();
                signal("emergency_stop");
            }
            if event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                != super::computer::INPUT_EVENT_TAG
                && !host_input
            {
                let pid = if target > 0 {
                    Some(target as u32)
                } else {
                    foreground_pid().map(|pid| pid as u32)
                };
                super::desktop_coordinator::user_input(
                    pid,
                    match kind {
                        CGEventType::KeyDown => super::desktop_coordinator::UserInput::Key,
                        CGEventType::ScrollWheel => super::desktop_coordinator::UserInput::Scroll,
                        CGEventType::LeftMouseDragged
                        | CGEventType::RightMouseDragged
                        | CGEventType::OtherMouseDragged => {
                            super::desktop_coordinator::UserInput::Drag
                        }
                        _ => super::desktop_coordinator::UserInput::Button,
                    },
                );
            }
            CallbackResult::Keep
        },
        || {
            AVAILABLE.store(true, Ordering::SeqCst);
            MONITOR_AVAILABLE.store(true, Ordering::SeqCst);
            unsafe { CFRunLoopRun() }
        },
    );
}
#[cfg(target_os = "windows")]
fn run() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    fn input_pid(point: Option<windows_sys::Win32::Foundation::POINT>) -> u32 {
        let window = unsafe {
            if let Some(point) = point {
                WindowFromPoint(point)
            } else {
                GetForegroundWindow()
            }
        };
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(window, &mut pid);
        }
        pid
    }
    unsafe extern "system" fn keyboard(code: i32, wparam: usize, lparam: isize) -> isize {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        if code >= 0 && (wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize) {
            let event = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            if event.dwExtraInfo != super::computer::INPUT_EVENT_TAG as usize
                && input_pid(None) != std::process::id()
            {
                super::desktop_coordinator::user_input(
                    Some(input_pid(None)),
                    super::desktop_coordinator::UserInput::Key,
                );
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
    unsafe extern "system" fn mouse(code: i32, wparam: usize, lparam: isize) -> isize {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let dragging = wparam == WM_MOUSEMOVE as usize
            && unsafe {
                use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
                GetAsyncKeyState(VK_LBUTTON as i32) < 0
                    || GetAsyncKeyState(VK_RBUTTON as i32) < 0
                    || GetAsyncKeyState(VK_MBUTTON as i32) < 0
            };
        if code >= 0
            && (dragging
                || matches!(
                    wparam as u32,
                    WM_LBUTTONDOWN
                        | WM_RBUTTONDOWN
                        | WM_MBUTTONDOWN
                        | WM_MOUSEWHEEL
                        | WM_MOUSEHWHEEL
                ))
        {
            let event = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
            if event.dwExtraInfo != super::computer::INPUT_EVENT_TAG as usize
                && input_pid(Some(event.pt)) != std::process::id()
            {
                super::desktop_coordinator::user_input(
                    Some(input_pid(Some(event.pt))),
                    if dragging {
                        super::desktop_coordinator::UserInput::Drag
                    } else if matches!(wparam as u32, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
                        super::desktop_coordinator::UserInput::Scroll
                    } else {
                        super::desktop_coordinator::UserInput::Button
                    },
                );
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }
    unsafe {
        let keyboard_hook =
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), std::ptr::null_mut(), 0);
        let mouse_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), std::ptr::null_mut(), 0);
        let hotkey = RegisterHotKey(
            std::ptr::null_mut(),
            0x4543,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            0x1B,
        ) != 0;
        AVAILABLE.store(hotkey, Ordering::SeqCst);
        MONITOR_AVAILABLE.store(
            !keyboard_hook.is_null() && !mouse_hook.is_null(),
            Ordering::SeqCst,
        );
        let mut message = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            if message.message == WM_HOTKEY && message.wParam == 0x4543 {
                super::desktop_coordinator::interrupt_current();
                signal("emergency_stop");
            }
        }
        if !keyboard_hook.is_null() {
            UnhookWindowsHookEx(keyboard_hook);
        }
        if !mouse_hook.is_null() {
            UnhookWindowsHookEx(mouse_hook);
        }
        let _ = UnregisterHotKey(std::ptr::null_mut(), 0x4543);
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn run() {}
