#[path = "macos_capture.rs"]
mod capture;
use super::{CancellationToken, Listing, Native, Node};
use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    ffi::c_void,
    ptr,
    time::{Duration, Instant},
};

type Raw = *const c_void;
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Raw;
    fn AXUIElementCreateSystemWide() -> Raw;
    fn AXUIElementCopyElementAtPosition(e: Raw, x: f32, y: f32, output: *mut Raw) -> i32;
    fn AXUIElementCopyAttributeValue(e: Raw, a: Raw, v: *mut Raw) -> i32;
    fn AXUIElementCopyAttributeValues(e: Raw, a: Raw, start: isize, max: isize, v: *mut Raw)
        -> i32;
    fn AXUIElementGetAttributeValueCount(e: Raw, a: Raw, count: *mut isize) -> i32;
    fn AXUIElementCopyActionNames(e: Raw, v: *mut Raw) -> i32;
    fn AXUIElementIsAttributeSettable(e: Raw, a: Raw, v: *mut u8) -> i32;
    fn AXUIElementSetAttributeValue(e: Raw, a: Raw, v: Raw) -> i32;
    fn AXUIElementPerformAction(e: Raw, a: Raw) -> i32;
    fn AXUIElementSetMessagingTimeout(e: Raw, timeout: f32) -> i32;
    fn AXValueGetValue(v: Raw, t: u32, result: *mut c_void) -> bool;
    fn AXValueCreate(t: u32, value: *const c_void) -> Raw;
    fn AXValueGetTypeID() -> usize;
    fn AXUIElementGetPid(e: Raw, pid: *mut i32) -> i32;
    fn AXUIElementGetTypeID() -> usize;
    fn CGWindowListCopyWindowInfo(options: u32, relative: u32) -> Raw;
    fn CGSessionCopyCurrentDictionary() -> Raw;
    fn CGGetActiveDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
    static kCGWindowOwnerPID: Raw;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(v: Raw) -> Raw;
    fn CFRelease(v: Raw);
    fn CFEqual(a: Raw, b: Raw) -> bool;
    fn CFGetTypeID(v: Raw) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(v: Raw) -> bool;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(v: Raw, kind: isize, result: *mut c_void) -> bool;
    #[cfg(test)]
    fn CFNumberCreate(allocator: Raw, kind: isize, value: *const c_void) -> Raw;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(v: Raw) -> isize;
    fn CFArrayGetValueAtIndex(v: Raw, i: isize) -> Raw;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: Raw, key: Raw) -> Raw;
    fn CFStringCreateWithCharacters(a: Raw, s: *const u16, n: isize) -> Raw;
    fn CFStringGetLength(v: Raw) -> isize;
    fn CFStringGetCharacters(v: Raw, r: Range, b: *mut u16);
    static kCFBooleanTrue: Raw;
    static kCFBooleanFalse: Raw;
}
#[repr(C)]
struct Range {
    location: isize,
    length: isize,
}
#[repr(C)]
#[derive(Default)]
struct Pair {
    a: f64,
    b: f64,
}
#[derive(Debug)]
struct Owned(Raw);
impl Clone for Owned {
    fn clone(&self) -> Self {
        Self(unsafe { CFRetain(self.0) })
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}
fn owned(raw: Raw) -> Result<Owned, String> {
    if raw.is_null() {
        Err("native_failure: null AX object".into())
    } else {
        Ok(Owned(raw))
    }
}
fn cf(value: &str) -> Owned {
    let s: Vec<u16> = value.encode_utf16().collect();
    owned(unsafe { CFStringCreateWithCharacters(ptr::null(), s.as_ptr(), s.len() as isize) })
        .expect("CFString allocation")
}
fn check(code: i32) -> Result<(), String> {
    match code {
        0 => Ok(()),
        -25211 => Err(api_disabled(unsafe { AXIsProcessTrusted() })),
        -25202 => Err("stale_reference: AX element no longer exists".into()),
        -25204 => {
            Err("native_timeout: AX provider did not respond (action result may be unknown)".into())
        }
        -25205 | -25206 | -25212 => Err(format!("unsupported: AX provider code {code}")),
        _ => Err(format!("native_failure: AX code {code}")),
    }
}
fn api_disabled(process_trusted: bool) -> String {
    if process_trusted {
        "accessibility_unavailable: this application/provider disabled its AX API; EchoAgent process is still AX-trusted; inspect another provider or a scoped appRef".into()
    } else {
        "permission_denied: Accessibility permission missing/revoked for this EchoAgent process"
            .into()
    }
}
fn exposed_descendant<T: Clone>(
    root: T,
    target: &T,
    mut children: impl FnMut(&T) -> Result<Vec<T>, String>,
    equal: impl Fn(&T, &T) -> bool,
) -> Result<bool, String> {
    let mut pending = std::collections::VecDeque::from([(root, 0)]);
    let start = Instant::now();
    let mut visited = 0;
    while let Some((node, depth)) = pending.pop_front() {
        if visited >= 256 || start.elapsed() > Duration::from_millis(800) {
            break;
        }
        visited += 1;
        if equal(&node, target) {
            return Ok(true);
        }
        if depth < 16 {
            for child in children(&node)? {
                pending.push_back((child, depth + 1));
            }
        }
    }
    Ok(false)
}
fn check_at(code: i32, operation: &str) -> Result<(), String> {
    check(code).map_err(|error| format!("{error}; {operation}"))
}
fn capability_result(code: i32, operation: &str, errors: &mut Vec<String>) -> Result<bool, String> {
    match code {
        0 => Ok(true),
        -25205 | -25206 | -25212 => Ok(false),
        // Optional capability queries can fail on otherwise readable controls.
        // Advertise no capability; keep the error visible without losing siblings.
        -25200 | -25208 => {
            if errors.len() < 16 {
                errors.push(format!("{}; {operation}", check(code).unwrap_err()));
            }
            Ok(false)
        }
        _ => check_at(code, operation).map(|_| false),
    }
}
fn settable(e: Raw, key: &str, errors: &mut Vec<String>) -> Result<bool, String> {
    let mut value = 0u8;
    let code = unsafe { AXUIElementIsAttributeSettable(e, cf(key).0, &mut value) };
    Ok(capability_result(code, &format!("settable({key})"), errors)? && value != 0)
}
fn attr(e: Raw, key: &str) -> Result<Option<Owned>, String> {
    if !unsafe { AXIsProcessTrusted() } {
        return Err("permission_denied: grant Accessibility to the actual EchoAgent app".into());
    }
    check_at(
        unsafe {
            AXUIElementSetMessagingTimeout(
                e,
                if matches!(
                    key,
                    "AXRole"
                        | "AXWindows"
                        | "AXFocusedWindow"
                        | "AXMainWindow"
                        | "AXFocusedApplication"
                ) {
                    0.5
                } else {
                    0.15
                },
            )
        },
        "AXUIElementSetMessagingTimeout",
    )?;
    let mut raw = ptr::null();
    let code = unsafe { AXUIElementCopyAttributeValue(e, cf(key).0, &mut raw) };
    if [-25205, -25212].contains(&code) {
        return Ok(None);
    }
    check_at(code, &format!("read({key})"))?;
    Ok(if raw.is_null() {
        None
    } else {
        Some(Owned(raw))
    })
}
fn text(v: &Owned, max: usize) -> Option<(String, bool)> {
    if unsafe { CFGetTypeID(v.0) != CFStringGetTypeID() } {
        return None;
    }
    let length = unsafe { CFStringGetLength(v.0) }.max(0) as usize;
    let mut units = vec![0u16; length.min(max)];
    unsafe {
        CFStringGetCharacters(
            v.0,
            Range {
                location: 0,
                length: units.len() as isize,
            },
            units.as_mut_ptr(),
        )
    };
    // A bounded UTF-16 copy must not manufacture a replacement character at
    // the boundary between an emoji's two surrogate units.
    if length > max && units.last().is_some_and(|u| (0xD800..=0xDBFF).contains(u)) {
        units.pop();
    }
    Some((String::from_utf16_lossy(&units), length > max))
}
fn readable(v: &Owned, max: usize) -> Option<(String, bool)> {
    if let Some(text) = text(v, max) {
        return Some(text);
    }
    let kind = unsafe { CFGetTypeID(v.0) };
    if kind == unsafe { CFBooleanGetTypeID() } {
        return Some((unsafe { CFBooleanGetValue(v.0) }.to_string(), false));
    }
    if kind == unsafe { CFNumberGetTypeID() } {
        let mut number = 0f64;
        if unsafe { CFNumberGetValue(v.0, 6, &mut number as *mut _ as *mut c_void) }
            && number.is_finite()
        {
            return Some((number.to_string(), false));
        }
    }
    None
}
fn string(e: Raw, key: &str, max: usize) -> Result<String, String> {
    Ok(attr(e, key)?
        .and_then(|v| text(&v, max))
        .map(|v| v.0)
        .unwrap_or_default())
}
fn metadata_result(
    result: Result<Option<Owned>, String>,
    errors: &mut Vec<String>,
) -> Result<(String, bool), String> {
    match result {
        Ok(value) => Ok(value.and_then(|v| text(&v, 1024)).unwrap_or_default()),
        Err(error) if error.starts_with("native_failure: AX code -25200;") => {
            if errors.len() < 16 {
                errors.push(error);
            }
            Ok((String::new(), true))
        }
        Err(error) => Err(error),
    }
}
fn boolean(e: Raw, key: &str) -> Result<Option<bool>, String> {
    Ok(attr(e, key)?
        .filter(|v| unsafe { CFGetTypeID(v.0) == CFBooleanGetTypeID() })
        .map(|v| unsafe { CFBooleanGetValue(v.0) }))
}
fn optional_state(
    e: Raw,
    key: &str,
    errors: &mut Vec<String>,
) -> Result<(Option<bool>, bool), String> {
    match boolean(e, key) {
        Ok(value) => Ok((value, false)),
        Err(error) if error.starts_with("native_failure: AX code -25200;") => {
            if errors.len() < 16 {
                errors.push(error);
            }
            Ok((None, true))
        }
        Err(error) => Err(error),
    }
}
fn array(v: &Owned, max: usize) -> Vec<Owned> {
    if unsafe { CFGetTypeID(v.0) != CFArrayGetTypeID() } {
        return vec![];
    }
    (0..unsafe { CFArrayGetCount(v.0) }.min(max as isize))
        .filter_map(|i| owned(unsafe { CFRetain(CFArrayGetValueAtIndex(v.0, i)) }).ok())
        .collect()
}

// TCC consent can remain granted during clamshell sleep/DarkWake. Check the
// actual graphical seat independently before trusting AX or sending input.
pub(super) fn desktop_available() -> Result<(), String> {
    let session = owned(unsafe { CGSessionCopyCurrentDictionary() })
        .map_err(|_| "desktop_unavailable: macOS graphical session is unavailable".to_string())?;
    if unsafe { CFGetTypeID(session.0) != CFDictionaryGetTypeID() } {
        return Err("desktop_unavailable: macOS session state is unavailable".into());
    }
    let flag = |key: &str| {
        let value = unsafe { CFDictionaryGetValue(session.0, cf(key).0) };
        if value.is_null() || unsafe { CFGetTypeID(value) != CFBooleanGetTypeID() } {
            None
        } else {
            Some(unsafe { CFBooleanGetValue(value) })
        }
    };
    let mut count = 0;
    let code = unsafe { CGGetActiveDisplayList(0, ptr::null_mut(), &mut count) };
    check_desktop_state(
        flag("CGSSessionScreenIsLocked"),
        flag("kCGSSessionOnConsoleKey"),
        flag("kCGSessionLoginDoneKey"),
        code == 0 && count > 0,
    )
}
fn check_desktop_state(
    locked: Option<bool>,
    console: Option<bool>,
    logged_in: Option<bool>,
    displays: bool,
) -> Result<(), String> {
    if locked != Some(true) && console == Some(true) && logged_in == Some(true) && displays {
        Ok(())
    } else {
        Err("desktop_unavailable: macOS desktop is asleep, locked or disconnected; restore the local console before refreshing controls; no input dispatched".into())
    }
}
fn window_role(role: &str) -> Result<(), String> {
    if role == "AXWindow" {
        Ok(())
    } else {
        Err(format!(
            "accessibility_unavailable: AXWindows returned role {role:?}, not a window"
        ))
    }
}

fn element_attr(e: Raw, key: &str) -> Result<Option<Owned>, String> {
    let value = attr(e, key)?;
    if value
        .as_ref()
        .is_some_and(|v| unsafe { CFGetTypeID(v.0) != AXUIElementGetTypeID() })
    {
        return Err(format!(
            "native_failure: read({key}) returned a non-element"
        ));
    }
    Ok(value)
}

fn observed_value(
    result: Result<Option<Owned>, String>,
    errors: &mut Vec<String>,
) -> Result<Option<Owned>, String> {
    match result {
        Err(error)
            if error.starts_with("native_failure: AX code -25200")
                || error.starts_with("native_failure: AX code -25208") =>
        {
            errors.push(error);
            Ok(None)
        }
        result => result,
    }
}

fn belongs_to_window<T: Clone>(
    element: &T,
    window: &T,
    direct_window: Option<T>,
    mut parent: impl FnMut(&T) -> Result<Option<T>, String>,
    equal: impl Fn(&T, &T) -> bool,
) -> Result<bool, String> {
    if equal(element, window) {
        return Ok(true);
    }
    // A contradictory AXWindow is a changed target, never a reason to search
    // another route. Some sheet controls omit AXWindow; verify their live
    // native parent chain rather than trusting their discovery-time position.
    if let Some(actual) = direct_window {
        return Ok(equal(&actual, window));
    }
    let start = Instant::now();
    let mut seen = vec![element.clone()];
    for _ in 0..32 {
        if start.elapsed() >= super::BUDGET {
            return Err("native_timeout: AX parent verification budget exceeded".into());
        }
        let Some(next) = parent(seen.last().unwrap())? else {
            return Ok(false);
        };
        if equal(&next, window) {
            return Ok(true);
        }
        if seen.iter().any(|previous| equal(previous, &next)) {
            return Err("stale_reference: cyclic AX parent chain".into());
        }
        seen.push(next);
    }
    Err("stale_reference: AX parent chain exceeds verification limit".into())
}

fn window_focus_matches<T: Clone>(
    focused: &T,
    window: &T,
    role: &str,
    direct_window: Option<T>,
    parent: impl FnMut(&T) -> Result<Option<T>, String>,
    equal: impl Fn(&T, &T) -> bool,
) -> Result<bool, String> {
    if equal(focused, window) {
        return Ok(true);
    }
    if role != "AXSheet" {
        return Ok(false);
    }
    belongs_to_window(focused, window, direct_window, parent, equal)
}

#[derive(Clone)]
pub struct Element {
    raw: Owned,
    // Host application and actual AX provider may differ for system panels.
    // Retain both identities; never re-resolve a replacement process at use.
    app: Retained<NSRunningApplication>,
    provider: Result<Retained<NSRunningApplication>, String>,
}
impl Element {
    fn for_host(raw: Owned, app: Retained<NSRunningApplication>) -> Self {
        let provider = (|| {
            let mut pid = 0;
            check_at(
                unsafe { AXUIElementGetPid(raw.0, &mut pid) },
                "AXUIElementGetPid",
            )?;
            if pid == app.processIdentifier() {
                return Ok(app.clone());
            }
            NSRunningApplication::runningApplicationWithProcessIdentifier(pid).ok_or_else(|| {
                "accessibility_unavailable: hosted AX provider identity unavailable".into()
            })
        })();
        Self { raw, app, provider }
    }
}
pub struct Backend;
#[cfg(test)]
pub(super) fn element_pid(e: &Element) -> i32 {
    e.app.processIdentifier()
}
pub(super) fn foreground_pid() -> Result<Option<i32>, String> {
    // The helper owns no AppKit application run loop. Workspace's cached
    // frontmostApplication can remain stale there; query the OS process focus
    // synchronously, then verify window/control through the target's AX tree.
    #[repr(C)]
    struct ProcessSerialNumber {
        high: u32,
        low: u32,
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn GetFrontProcess(process: *mut ProcessSerialNumber) -> i16;
        fn GetProcessPID(process: *const ProcessSerialNumber, pid: *mut i32) -> i32;
    }
    let mut process = ProcessSerialNumber { high: 0, low: 0 };
    let mut pid = 0;
    if unsafe { GetFrontProcess(&mut process) } != 0
        || unsafe { GetProcessPID(&process, &mut pid) } != 0
    {
        return Err(
            "focus_unavailable: OS foreground process unavailable; no input dispatched".into(),
        );
    }
    Ok((pid > 0).then_some(pid))
}

/// Use the app's current, unique advertised menu item for command shortcuts.
/// Inactive AppKit applications can accept text events but ignore command-key
/// equivalents; native menu actions preserve background execution semantics.
fn invoke_menu_shortcut(
    app: Raw,
    key: &str,
    modifiers: &[String],
    token: &CancellationToken,
) -> Result<bool, String> {
    if key.chars().count() != 1 || modifiers.is_empty() {
        return Ok(false);
    }
    let mut mask = 8u32;
    for modifier in modifiers {
        match modifier.as_str() {
            "cmd" | "command" | "meta" => mask &= !8,
            "ctrl" | "control" => mask |= 4,
            "alt" | "option" => mask |= 2,
            "shift" => mask |= 1,
            _ => return Err("invalid_argument: unknown shortcut modifier".into()),
        }
    }
    let Some(menu) = element_attr(app, "AXMenuBar")? else {
        return Ok(false);
    };
    let mut pending = vec![(menu, 0usize)];
    let mut matches = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut visited = 0;
    while let Some((node, depth)) = pending.pop() {
        if token.is_cancelled() {
            return Err("cancelled: no shortcut dispatched".into());
        }
        if visited >= 1000 || Instant::now() >= deadline {
            return Err("incomplete_menu: cannot verify unique background shortcut; use observed control action".into());
        }
        visited += 1;
        if string(node.0, "AXRole", 128)? == "AXMenuItem" {
            let character = string(node.0, "AXMenuItemCmdChar", 16)?;
            let flags = attr(node.0, "AXMenuItemCmdModifiers")?
                .and_then(|v| readable(&v, 32))
                .and_then(|v| v.0.parse::<f64>().ok())
                .unwrap_or(0.0) as u32;
            if character.eq_ignore_ascii_case(key)
                && flags == mask
                && boolean(node.0, "AXEnabled")? == Some(true)
            {
                matches.push(node.clone());
            }
        }
        if depth < 8 {
            if let Some(children) = attr(node.0, "AXChildren")? {
                for child in array(&children, 128) {
                    pending.push((child, depth + 1));
                }
            }
        }
    }
    if matches.len() > 1 {
        return Err(
            "ambiguous_shortcut: multiple enabled native menu items; no action dispatched".into(),
        );
    }
    if let Some(item) = matches.first() {
        if token.is_cancelled() {
            return Err("cancelled: no shortcut dispatched".into());
        }
        check(unsafe { AXUIElementPerformAction(item.0, cf("AXPress").0) })?;
        return Ok(true);
    }
    Ok(false)
}

pub(super) fn pump_capture_events() {
    refresh_workspace_events();
}

fn refresh_workspace_events() {
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopRunInMode(mode: Raw, seconds: f64, return_after_source: u8) -> i32;
        static kCFRunLoopDefaultMode: Raw;
    }
    // A pipe-driven helper must let Workspace refresh process metadata without
    // installing an NSApplication or a second UI event loop.
    for _ in 0..4 {
        if unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1) } != 4 {
            break;
        }
    }
}
fn visible_app_order() -> HashMap<i32, usize> {
    let mut order = HashMap::new();
    // Read only owner PIDs in front-to-back order, never window contents.
    // This is an optional prioritization hint; AX still supplies every result.
    if let Ok(windows) = owned(unsafe { CGWindowListCopyWindowInfo(1 | 16, 0) }) {
        for window in array(&windows, 1024) {
            if unsafe { CFGetTypeID(window.0) } != unsafe { CFDictionaryGetTypeID() } {
                continue;
            }
            let value = unsafe { CFDictionaryGetValue(window.0, kCGWindowOwnerPID) };
            let mut pid = 0i32;
            if !value.is_null()
                && unsafe {
                    CFGetTypeID(value) == CFNumberGetTypeID()
                        && CFNumberGetValue(value, 3, &mut pid as *mut _ as *mut c_void)
                }
            {
                let next = order.len();
                order.entry(pid).or_insert(next);
            }
        }
    }
    order
}
impl Backend {
    fn has_window_focus(&self, window: &Element) -> Result<bool, String> {
        Ok(foreground_pid()? == Some(window.app.processIdentifier())
            && self.has_app_window_focus(window)?)
    }
    fn has_app_window_focus(&self, window: &Element) -> Result<bool, String> {
        self.alive(window)?;
        let app = owned(unsafe { AXUIElementCreateApplication(window.app.processIdentifier()) })?;
        let Some(raw) = element_attr(app.0, "AXFocusedWindow")? else {
            return Ok(false);
        };
        let focused = Element::for_host(raw, window.app.clone());
        self.alive(&focused)?;
        if self.same_element(&focused, window) {
            return Ok(true);
        }
        window_focus_matches(
            &focused.raw,
            &window.raw,
            &string(focused.raw.0, "AXRole", 128)?,
            element_attr(focused.raw.0, "AXWindow")?,
            |e| element_attr(e.0, "AXParent"),
            |a, b| unsafe { CFEqual(a.0, b.0) },
        )
    }
    fn alive(&self, e: &Element) -> Result<(), String> {
        if e.app.isTerminated() {
            return Err("application_exited: rediscover application".into());
        }
        let provider = e.provider.as_ref().map_err(Clone::clone)?;
        if provider.isTerminated() {
            return Err("stale_reference: original AX provider exited".into());
        }
        let mut pid = 0;
        check(unsafe { AXUIElementGetPid(e.raw.0, &mut pid) })?;
        if pid != provider.processIdentifier() {
            return Err("stale_reference: process identity changed".into());
        }
        Ok(())
    }
}
impl Native for Backend {
    fn application_bundle_path(&self, element: &Element) -> Option<std::path::PathBuf> {
        let path = element.app.bundleURL()?.path()?.to_string();
        std::fs::canonicalize(path).ok()
    }
    fn hit_target(&self, window: &Element, x: f64, y: f64) -> Result<Element, String> {
        let root = owned(unsafe { AXUIElementCreateSystemWide() })?;
        let mut hit = ptr::null();
        let status =
            unsafe { AXUIElementCopyElementAtPosition(root.0, x as f32, y as f32, &mut hit) };
        if status == -25208 {
            // Some custom-drawn apps expose windows but not AX point lookup.
            // WindowServer must independently prove the exact topmost window.
            capture::verify_point(self, window, x, y)?;
            return Ok(window.clone());
        }
        check(status)?;
        let hit = Element::for_host(owned(hit)?, window.app.clone());
        if let Err(error) = self.validate(&hit, Some(window)) {
            if error.starts_with("stale_reference: control has no verified path")
                && self.process_id(&hit) == Some(window.app.processIdentifier() as u32)
                && !self.inspect(&hit)?.protected
                && capture::verify_point(self, window, x, y)?
            {
                // A verified same-app transient is represented in the bound
                // parent frame. Keep the parent identity for physical input.
                return Ok(window.clone());
            }
            return Err(error);
        }
        Ok(hit)
    }

    fn foreground_window(&self) -> Result<Element, String> {
        self.check_session()?;
        let pid = foreground_pid()?.ok_or("focus_unavailable: no foreground application")?;
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .ok_or("application_exited")?;
        let raw = owned(unsafe { AXUIElementCreateApplication(pid) })?;
        let window = Element::for_host(
            element_attr(raw.0, "AXFocusedWindow")?.ok_or("focus_unavailable: no window")?,
            app.clone(),
        );
        Ok(window)
    }
    fn keyboard_focus(&self, window: &Element) -> Result<Option<Element>, String> {
        self.alive(window)?;
        let raw = owned(unsafe { AXUIElementCreateApplication(window.app.processIdentifier()) })?;
        element_attr(raw.0, "AXFocusedUIElement")?
            .map(|raw| {
                let focused = Element::for_host(raw, window.app.clone());
                self.validate(&focused, Some(window))?;
                Ok(focused)
            })
            .transpose()
    }
    fn focused_target(&self) -> Result<(Element, Element), String> {
        let window = self.foreground_window()?;
        let focus = self
            .keyboard_focus(&window)?
            .ok_or("focus_unavailable: no control")?;
        Ok((window, focus))
    }

    fn application_window(&self, app: &Element) -> Result<Option<Element>, String> {
        let Some(raw) = element_attr(app.raw.0, "AXFocusedWindow")? else {
            return Ok(None);
        };
        let current = Element::for_host(raw, app.app.clone());
        if string(current.raw.0, "AXRole", 128)? == "AXSheet" {
            return Ok(element_attr(current.raw.0, "AXWindow")?
                .map(|raw| Element::for_host(raw, app.app.clone())));
        }
        Ok(Some(current))
    }
    fn file_dialog(
        &self,
        window: &Element,
    ) -> Result<Option<super::file_dialog::Dialog<Element>>, String> {
        let root = self.observation_root(window)?;
        if string(root.raw.0, "AXRole", 128)? != "AXSheet" {
            return Ok(None);
        }
        let kind = match string(root.raw.0, "AXIdentifier", 128)?.as_str() {
            "save-panel" => "save",
            "open-panel" => "open",
            "GoToWindow" => "goto",
            _ => return Ok(None),
        };
        let mut pending = std::collections::VecDeque::from([root.clone()]);
        let mut path = None;
        let mut filename = None;
        let mut count = 0;
        let deadline = Instant::now() + Duration::from_millis(700);
        while let Some(element) = pending.pop_front() {
            count += 1;
            if count > 128 || Instant::now() > deadline {
                break;
            }
            match string(element.raw.0, "AXIdentifier", 256)?.as_str() {
                "PathTextField" => path = Some(element.clone()),
                "saveAsNameTextField" => filename = Some(element.clone()),
                _ => {}
            }
            if (kind == "goto" && path.is_some())
                || (kind == "save" && filename.is_some())
                || kind == "open"
            {
                break;
            }
            if let Some(children) = attr(element.raw.0, "AXChildren")? {
                for child in array(&children, 128 - count) {
                    pending.push_back(Element::for_host(child, window.app.clone()));
                }
            }
        }
        Ok(Some(super::file_dialog::Dialog {
            kind,
            root,
            path,
            filename,
        }))
    }
    type Element = Element;
    fn same_element(&self, a: &Element, b: &Element) -> bool {
        if unsafe { CFEqual(a.raw.0, b.raw.0) } {
            return true;
        }
        // Some providers recreate AXWindow proxies during ordinary editing.
        // Only windows may use this fallback, backed by an exact native ID.
        if a.app.processIdentifier() != b.app.processIdentifier()
            || string(a.raw.0, "AXRole", 128).ok().as_deref() != Some("AXWindow")
            || string(b.raw.0, "AXRole", 128).ok().as_deref() != Some("AXWindow")
        {
            return false;
        }
        match (capture::window_id(self, a), capture::window_id(self, b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
    fn application_element(&self, e: &Element) -> Result<Option<Element>, String> {
        self.alive(e)?;
        Ok(Some(Element::for_host(
            owned(unsafe { AXUIElementCreateApplication(e.app.processIdentifier()) })?,
            e.app.clone(),
        )))
    }
    fn commands(&self, e: &Element) -> Result<Option<Vec<Value>>, String> {
        self.alive(e)?;
        let Some(menu) = element_attr(e.raw.0, "AXMenuBar")? else {
            return Ok(None);
        };
        let start = Instant::now();
        let mut pending = vec![(menu, Vec::<String>::new(), 0usize)];
        let mut output = Vec::new();
        let mut visited = 0;
        while let Some((node, path, depth)) = pending.pop() {
            if visited >= 450 || output.len() >= 64 || start.elapsed() > Duration::from_millis(700)
            {
                break;
            }
            visited += 1;
            let title = string(node.0, "AXTitle", 128)?;
            if title == "Apple" {
                continue;
            }
            let mut path = path;
            if !title.is_empty() && path.last() != Some(&title) {
                path.push(title);
            }
            if string(node.0, "AXRole", 128)? == "AXMenuItem" {
                let key = string(node.0, "AXMenuItemCmdChar", 16)?;
                let modifiers = attr(node.0, "AXMenuItemCmdModifiers")?
                    .and_then(|v| readable(&v, 32))
                    .and_then(|v| v.0.parse::<f64>().ok())
                    .unwrap_or(0.0) as u32;
                if key.chars().count() == 1
                    && key.bytes().all(|c| c.is_ascii_alphanumeric())
                    && modifiers & !15 == 0
                {
                    let mut parts = Vec::new();
                    if modifiers & 8 == 0 {
                        parts.push("cmd".to_string());
                    }
                    if modifiers & 4 != 0 {
                        parts.push("ctrl".into());
                    }
                    if modifiers & 2 != 0 {
                        parts.push("alt".into());
                    }
                    if modifiers & 1 != 0 {
                        parts.push("shift".into());
                    }
                    parts.push(key.to_ascii_lowercase());
                    output.push(json!({"label":path.join(" > "),"shortcut":parts.join("+"),"enabled":boolean(node.0,"AXEnabled")?.unwrap_or(false)}));
                }
            }
            if depth < 5 {
                if let Some(children) = attr(node.0, "AXChildren")? {
                    for child in array(&children, 120).into_iter().rev() {
                        pending.push((child, path.clone(), depth + 1));
                    }
                }
            }
        }
        Ok(Some(output))
    }
    fn observation_root(&self, window: &Element) -> Result<Element, String> {
        {
            let app =
                owned(unsafe { AXUIElementCreateApplication(window.app.processIdentifier()) })?;
            if let Some(raw) = element_attr(app.0, "AXFocusedWindow")? {
                let focused = Element::for_host(raw, window.app.clone());
                if string(focused.raw.0, "AXRole", 128)? == "AXSheet" {
                    // The focused sheet must still belong to this exact retained
                    // document window, including nested system-provider sheets.
                    self.validate(&focused, Some(window))?;
                    return Ok(focused);
                }
            }
        }
        Ok(window.clone())
    }
    fn new() -> Result<Self, String> {
        Ok(Self)
    }
    fn check_session(&self) -> Result<(), String> {
        refresh_workspace_events();
        desktop_available()
    }
    fn open_app(&self, path: &str) -> Result<bool, String> {
        // LaunchServices' -g option opens without activating the application.
        if !std::process::Command::new("/usr/bin/open")
            .args(["-g", "-a", path])
            .status()
            .map_err(|e| format!("launch_failed: {e}"))?
            .success()
        {
            return Err("launch_failed: macOS refused application URL".into());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            refresh_workspace_events();
            if NSWorkspace::sharedWorkspace()
                .runningApplications()
                .iter()
                .any(|app| {
                    app.bundleURL().and_then(|url| url.path()).is_some_and(|p| {
                        std::fs::canonicalize(p.to_string()).ok().as_deref()
                            == Some(std::path::Path::new(path))
                    })
                })
            {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(false)
    }
    fn reopen_app(&self, path: &str) -> Result<bool, String> {
        if !std::process::Command::new("/usr/bin/open")
            .args(["-a", path])
            .status()
            .map_err(|e| format!("launch_failed: {e}"))?
            .success()
        {
            return Err("launch_failed: OS refused to reopen the application".into());
        }
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            refresh_workspace_events();
            for app in NSWorkspace::sharedWorkspace().runningApplications().iter() {
                let same = app.bundleURL().and_then(|url| url.path()).is_some_and(|p| {
                    std::fs::canonicalize(p.to_string()).ok().as_deref()
                        == Some(std::path::Path::new(path))
                });
                if same && foreground_pid()? == Some(app.processIdentifier()) {
                    return Ok(true);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(false)
    }
    fn apps(&self) -> Result<Listing<Element>, String> {
        let foreground = foreground_pid().ok().flatten();
        Ok(Listing::complete(
            NSWorkspace::sharedWorkspace()
                .runningApplications()
                .iter()
                .filter(|a| !a.isTerminated())
                .filter_map(|a| {
                    let raw = owned(unsafe { AXUIElementCreateApplication(a.processIdentifier()) })
                        .ok()?;
                    let name = a.localizedName().map(|s| s.to_string()).unwrap_or_default();
                    let identifier = a
                        .bundleIdentifier()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    Some((
                        Element {
                            raw,
                            app: a.clone(),
                            provider: Ok(a.clone()),
                        },
                        Node {
                            role: "application".into(),
                            name,
                            identifier,
                            aliases: a
                                .bundleURL()
                                .and_then(|u| u.path())
                                .and_then(|p| {
                                    std::path::Path::new(&p.to_string())
                                        .file_stem()
                                        .map(|s| s.to_string_lossy().into_owned())
                                })
                                .into_iter()
                                .collect(),
                            enabled: true,
                            visible: Some(!a.isHidden()),
                            focused: foreground == Some(a.processIdentifier()),
                            ..Node::default()
                        },
                    ))
                })
                .collect(),
        ))
    }
    fn windows(&self, app: Option<&Element>) -> Result<Listing<Element>, String> {
        if !unsafe { AXIsProcessTrusted() } {
            return Err(
                "permission_denied: grant Accessibility to the actual EchoAgent app".into(),
            );
        }
        let mut apps = if let Some(a) = app {
            vec![(a.clone(), Node::default())]
        } else {
            self.apps()?.items
        };
        let visible = visible_app_order();
        apps.sort_by_key(|(app, node)| {
            (
                !node.focused,
                visible
                    .get(&app.app.processIdentifier())
                    .copied()
                    .unwrap_or(usize::MAX),
                app.app.activationPolicy().0,
            )
        });
        let mut result = Listing::complete(vec![]);
        let start = Instant::now();
        'apps: for (app, _) in apps {
            let name = app
                .app
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if start.elapsed() > super::BUDGET {
                result.issues.push((
                    name,
                    "native_timeout: window enumeration budget exceeded; retry with appRef".into(),
                ));
                break;
            }
            let windows = self.alive(&app).and_then(|_| attr(app.raw.0, "AXWindows"));
            match windows {
                Ok(windows) => {
                    let mut native_windows = windows.map(|v| array(&v, 257)).unwrap_or_default();
                    // Providers may expose a live main/focused window before
                    // AXWindows updates (including a newly launched app).
                    for key in ["AXFocusedWindow", "AXMainWindow"] {
                        if let Ok(Some(candidate)) = element_attr(app.raw.0, key) {
                            let candidate = if string(candidate.0, "AXRole", 128).ok().as_deref()
                                == Some("AXSheet")
                            {
                                element_attr(candidate.0, "AXWindow")
                                    .ok()
                                    .flatten()
                                    .unwrap_or(candidate)
                            } else {
                                candidate
                            };
                            if !native_windows
                                .iter()
                                .any(|w| unsafe { CFEqual(w.0, candidate.0) })
                            {
                                native_windows.insert(0, candidate);
                            }
                        }
                    }
                    for raw in native_windows {
                        if unsafe { CFGetTypeID(raw.0) != AXUIElementGetTypeID() } {
                            result.issues.push((
                                name.clone(),
                                "accessibility_unavailable: AXWindows returned a non-element"
                                    .into(),
                            ));
                            continue;
                        }
                        if start.elapsed() > super::BUDGET || result.items.len() >= 257 {
                            result.issues.push((
                                name.clone(),
                                "discovery_limit: window enumeration bounded; retry with appRef"
                                    .into(),
                            ));
                            break 'apps;
                        }
                        let e = Element::for_host(raw, app.app.clone());
                        match self.inspect(&e) {
                            Ok(node) => match window_role(&node.role) {
                                Ok(()) => result.items.push((e, node)),
                                Err(error) => result.issues.push((name.clone(), error)),
                            },
                            Err(error) if error.starts_with("permission_denied") => {
                                return Err(error)
                            }
                            Err(error) => result.issues.push((name.clone(), error)),
                        }
                    }
                }
                Err(error) if error.starts_with("permission_denied") => return Err(error),
                Err(error) => result.issues.push((name, error)),
            }
        }
        Ok(result)
    }
    fn document_url(&self, e: &Element) -> Result<Option<String>, String> {
        let Some(value) = attr(e.raw.0, "AXDocument")? else {
            return Ok(None);
        };
        let Some((url, truncated)) = text(&value, 4096) else {
            return Ok(None);
        };
        if truncated {
            return Err("metadata_truncated: document URL unavailable in full".into());
        }
        // Expose local document locations, not website query strings/tokens.
        Ok((url.starts_with("file:///") && !url.contains(['?', '#'])).then_some(url))
    }

    fn application_identifier(&self, e: &Element) -> Option<String> {
        e.app
            .bundleIdentifier()
            .map(|identifier| identifier.to_string())
    }

    fn inspect(&self, e: &Element) -> Result<Node, String> {
        self.alive(e)?;
        let role = string(e.raw.0, "AXRole", 128)?;
        let subrole = string(e.raw.0, "AXSubrole", 128)?;
        let protected = subrole == "AXSecureTextField"
            || role == "AXSecureTextField"
            || boolean(e.raw.0, "AXProtectedContent")?.unwrap_or(false);
        let mut observation_errors = Vec::new();
        let (name, name_truncated) = if protected {
            ("[protected]".into(), false)
        } else {
            let title = metadata_result(attr(e.raw.0, "AXTitle"), &mut observation_errors)?;
            if title.0.is_empty() {
                let description =
                    metadata_result(attr(e.raw.0, "AXDescription"), &mut observation_errors)?;
                (description.0, title.1 || description.1)
            } else {
                title
            }
        };
        let (identifier, identifier_truncated) =
            metadata_result(attr(e.raw.0, "AXIdentifier"), &mut observation_errors)?;
        let raw_value = if protected {
            None
        } else {
            // A container can expose children while its optional AXValue query
            // fails. Preserve that subtree, advertise no readable/writable
            // value; explicit computer_read still requires a real value read.
            observed_value(attr(e.raw.0, "AXValue"), &mut observation_errors)?
        };
        let text_value = raw_value
            .as_ref()
            .is_some_and(|v| unsafe { CFGetTypeID(v.0) == CFStringGetTypeID() });
        let (value, text_truncated) = if protected {
            (None, false)
        } else {
            raw_value
                .as_ref()
                .and_then(|v| readable(v, 4096))
                .map(|(s, t)| (Some(s), t))
                .unwrap_or((None, false))
        };
        let writable = settable(e.raw.0, "AXValue", &mut observation_errors)?;
        let mut raw = ptr::null();
        let action_code = unsafe { AXUIElementCopyActionNames(e.raw.0, &mut raw) };
        let mut actions = vec![];
        for (attribute, names) in [
            ("AXFocused", vec!["focus"]),
            ("AXSelected", vec!["select"]),
            ("AXExpanded", vec!["expand", "collapse"]),
        ] {
            if settable(e.raw.0, attribute, &mut observation_errors)? {
                actions.extend(names.into_iter().map(str::to_string));
            }
        }
        if action_code == 0 && !raw.is_null() {
            for name in array(&Owned(raw), 64) {
                if let Some((name, _)) = text(&name, 128) {
                    actions.push(name);
                }
            }
        } else {
            capability_result(
                action_code,
                "AXUIElementCopyActionNames",
                &mut observation_errors,
            )?;
        }
        let bounds = match (
            observed_value(attr(e.raw.0, "AXPosition"), &mut observation_errors)?,
            observed_value(attr(e.raw.0, "AXSize"), &mut observation_errors)?,
        ) {
            (Some(p), Some(s))
                if unsafe {
                    CFGetTypeID(p.0) == AXValueGetTypeID() && CFGetTypeID(s.0) == AXValueGetTypeID()
                } =>
            {
                let mut p2 = Pair::default();
                let mut s2 = Pair::default();
                if unsafe {
                    AXValueGetValue(p.0, 1, &mut p2 as *mut _ as *mut c_void)
                        && AXValueGetValue(s.0, 2, &mut s2 as *mut _ as *mut c_void)
                } {
                    Some([p2.a, p2.b, s2.a, s2.b])
                } else {
                    None
                }
            }
            _ => None,
        };
        let focus = if role == "AXWindow" {
            self.has_window_focus(e)
        } else if role == "AXApplication" {
            foreground_pid().map(|pid| pid == Some(e.app.processIdentifier()))
        } else {
            Ok(
                optional_state(e.raw.0, "AXFocused", &mut observation_errors)?
                    .0
                    .unwrap_or(false),
            )
        };
        let focused = match focus {
            Ok(v) => v,
            Err(e) if e.starts_with("permission_denied") => return Err(e),
            Err(e) => {
                observation_errors.push(format!("focus unavailable: {e}"));
                false
            }
        };
        let (enabled, enabled_unreadable) =
            optional_state(e.raw.0, "AXEnabled", &mut observation_errors)?;
        let hidden = optional_state(e.raw.0, "AXHidden", &mut observation_errors)?.0;
        let selected = optional_state(e.raw.0, "AXSelected", &mut observation_errors)?.0;
        let expanded = optional_state(e.raw.0, "AXExpanded", &mut observation_errors)?.0;
        Ok(Node {
            aliases: vec![],
            application: e
                .app
                .localizedName()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            role,
            name,
            identifier,
            value,
            enabled: enabled.unwrap_or(!enabled_unreadable),
            visible: if hidden == Some(true) {
                Some(false)
            } else {
                None
            },
            focused,
            selected,
            expanded,
            protected,
            writable: writable && text_value && !protected,
            bounds,
            actions: if protected { vec![] } else { actions },
            text_truncated,
            metadata_truncated: name_truncated || identifier_truncated,
            observation_errors,
        })
    }
    fn children(&self, e: &Element, limit: usize) -> Result<(Vec<Element>, bool), String> {
        let key = cf("AXChildren");
        let mut count = 0;
        let code = unsafe { AXUIElementGetAttributeValueCount(e.raw.0, key.0, &mut count) };
        if [-25205, -25212].contains(&code) {
            return Ok((vec![], false));
        }
        check_at(code, "count(AXChildren)")?;
        if count <= 0 {
            return Ok((vec![], false));
        }
        let mut raw = ptr::null();
        check_at(
            unsafe {
                AXUIElementCopyAttributeValues(
                    e.raw.0,
                    key.0,
                    0,
                    count.min(limit as isize),
                    &mut raw,
                )
            },
            "read(AXChildren)",
        )?;
        Ok((
            array(&owned(raw)?, limit)
                .into_iter()
                .map(|raw| Element::for_host(raw, e.app.clone()))
                .collect(),
            count > limit as isize,
        ))
    }
    fn validate(&self, e: &Element, window: Option<&Element>) -> Result<(), String> {
        self.alive(e)?;
        if let Some(w) = window {
            self.alive(w)?;
            if !self.same_element(e, w) {
                let mut belongs = belongs_to_window(
                    &e.raw,
                    &w.raw,
                    element_attr(e.raw.0, "AXWindow")?,
                    |element| element_attr(element.0, "AXParent"),
                    |a, b| unsafe { CFEqual(a.0, b.0) },
                )?;
                if !belongs {
                    // Hosted system controls can omit AXParent/AXWindow links
                    // back to the host. Prove containment in the live host tree
                    // by exact AX identity instead; geometry is never proof.
                    let app =
                        owned(unsafe { AXUIElementCreateApplication(e.app.processIdentifier()) })?;
                    let root = if let Some(focused) = element_attr(app.0, "AXFocusedWindow")? {
                        if belongs_to_window(
                            &focused,
                            &w.raw,
                            element_attr(focused.0, "AXWindow")?,
                            |n| element_attr(n.0, "AXParent"),
                            |a, b| unsafe { CFEqual(a.0, b.0) },
                        )? {
                            focused
                        } else {
                            w.raw.clone()
                        }
                    } else {
                        w.raw.clone()
                    };
                    belongs = exposed_descendant(
                        root,
                        &e.raw,
                        |n| {
                            Ok(attr(n.0, "AXChildren")?
                                .map(|v| array(&v, 256))
                                .unwrap_or_default())
                        },
                        |a, b| unsafe { CFEqual(a.0, b.0) },
                    )?;
                }
                if !belongs {
                    return Err(
                        "stale_reference: control has no verified path to its original window"
                            .into(),
                    );
                }
            }
        }
        // Even a retained AX handle can outlive the underlying control.
        let _ = attr(e.raw.0, "AXRole")?
            .ok_or("accessibility_unavailable: provider exposes no role")?;
        Ok(())
    }
    fn activate(&self, e: &Element) -> Result<bool, String> {
        self.alive(e)?;
        let role = string(e.raw.0, "AXRole", 128)?;
        if foreground_pid()? == Some(e.app.processIdentifier())
            && (role != "AXWindow" || self.has_window_focus(e)?)
        {
            return Ok(true);
        }
        if foreground_pid()? != Some(e.app.processIdentifier()) {
            let app = owned(unsafe { AXUIElementCreateApplication(e.app.processIdentifier()) })?;
            // Use the provider's native foreground action as well as AppKit's
            // request. The actual global AX identity below decides success;
            // neither a cached isActive property nor dispatch return is proof.
            let _ =
                unsafe { AXUIElementSetAttributeValue(app.0, cf("AXFrontmost").0, kCFBooleanTrue) };
            let _ = e
                .app
                .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        }
        if role == "AXWindow" {
            check(unsafe { AXUIElementPerformAction(e.raw.0, cf("AXRaise").0) })?;
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            if foreground_pid()? == Some(e.app.processIdentifier()) {
                if role != "AXWindow" {
                    return Ok(true);
                }
                if self.has_window_focus(e)? {
                    return Ok(true);
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(false)
    }
    fn invoke(&self, e: &Element, action: &str) -> Result<(), String> {
        let attribute = match action {
            "focus" => Some("AXFocused"),
            "select" => Some("AXSelected"),
            "expand" | "collapse" => Some("AXExpanded"),
            _ => None,
        };
        if let Some(attribute) = attribute {
            check(unsafe {
                AXUIElementSetAttributeValue(
                    e.raw.0,
                    cf(attribute).0,
                    if action == "collapse" {
                        kCFBooleanFalse
                    } else {
                        kCFBooleanTrue
                    },
                )
            })
        } else {
            check(unsafe { AXUIElementPerformAction(e.raw.0, cf(action).0) })
        }
    }
    fn process_id(&self, e: &Element) -> Option<u32> {
        Some(e.app.processIdentifier() as u32)
    }
    fn process_ids(&self, e: &Element) -> Vec<u32> {
        let mut pids = vec![e.app.processIdentifier() as u32];
        if let Ok(provider) = &e.provider {
            let pid = provider.processIdentifier() as u32;
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
        pids
    }
    fn recognize_text(&self, png: &[u8]) -> Result<Vec<super::visual_text::Line>, String> {
        super::macos_vision::recognize(png)
    }
    fn capture_window(&self, e: &Element) -> Result<Value, String> {
        capture::capture(self, e)
    }

    fn prepare_input_window(&self, window: &Element) -> Result<bool, String> {
        if self.has_app_window_focus(window)? {
            return Ok(true);
        }
        let app = owned(unsafe { AXUIElementCreateApplication(window.app.processIdentifier()) })?;
        // Change only the application's own focused window. Never raise or
        // activate the app as a hidden fallback for a background request.
        check(unsafe { AXUIElementSetAttributeValue(app.0, cf("AXFocusedWindow").0, window.raw.0) })
            .map_err(|_| "background_focus_unavailable: target app cannot focus this window without activation; use another native action or request foreground access".to_string())?;
        self.has_app_window_focus(window)
    }
    fn select_all_text(
        &self,
        e: &Element,
        window: &Element,
        token: &CancellationToken,
    ) -> Result<(), String> {
        self.verify_text_focus(e, window)?;
        let mut settable = 0;
        check(unsafe {
            AXUIElementIsAttributeSettable(e.raw.0, cf("AXSelectedTextRange").0, &mut settable)
        })?;
        if settable == 0 {
            return self.key_cancellable(window, "a", &["cmd".into()], token);
        }
        let value =
            attr(e.raw.0, "AXValue")?.ok_or("unsupported_action: text length unavailable")?;
        if unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
            return Err("unsupported_action: text range requires string value".into());
        }
        let range = Range {
            location: 0,
            length: unsafe { CFStringGetLength(value.0) },
        };
        let selection = owned(unsafe { AXValueCreate(4, &range as *const _ as *const c_void) })?;
        if token.is_cancelled() {
            return Err("cancelled: no selection dispatched".into());
        }
        check(unsafe {
            AXUIElementSetAttributeValue(e.raw.0, cf("AXSelectedTextRange").0, selection.0)
        })?;
        let actual = attr(e.raw.0, "AXSelectedTextRange")?
            .ok_or("input_unverified: selected range unavailable")?;
        let mut observed = Range {
            location: -1,
            length: -1,
        };
        if !unsafe { AXValueGetValue(actual.0, 4, &mut observed as *mut _ as *mut c_void) }
            || observed.location != 0
            || observed.length != range.length
        {
            return Err(
                "input_unverified: select-all range did not match; no replacement input dispatched"
                    .into(),
            );
        }
        Ok(())
    }
    fn emit_text(&self, e: &Element, text: &str) -> Result<(), String> {
        self.alive(e)?;
        super::super::computer::targeted_text(
            e.provider
                .as_ref()
                .map_err(Clone::clone)?
                .processIdentifier(),
            text,
        )
    }
    fn key(&self, e: &Element, key: &str, modifiers: &[String]) -> Result<(), String> {
        self.key_cancellable(e, key, modifiers, &CancellationToken::new())
    }
    fn key_cancellable(
        &self,
        e: &Element,
        key: &str,
        modifiers: &[String],
        token: &CancellationToken,
    ) -> Result<(), String> {
        if token.is_cancelled() {
            return Err("cancelled: no key dispatched".into());
        }
        self.alive(e)?;
        let app = owned(unsafe { AXUIElementCreateApplication(e.app.processIdentifier()) })?;
        let focused = self.keyboard_focus(e)?;
        if focused
            .as_ref()
            .map(|f| self.inspect(f))
            .transpose()?
            .is_some_and(|n| n.protected)
        {
            return Err("protected: keyboard focus requires user takeover".into());
        }
        if !self.has_app_window_focus(e)? {
            return Err(
                "focus_changed: approved window is no longer foreground; key not dispatched".into(),
            );
        }
        if invoke_menu_shortcut(app.0, key, modifiers, token)? {
            return Ok(());
        }
        let focused = focused.ok_or("visual_fallback_required: native keyboard focus unavailable; activate this window, capture it with computer_screenshot(windowRef), and use foreground visual input")?;
        if token.is_cancelled() {
            return Err("cancelled: no key dispatched".into());
        }
        super::super::computer::targeted_key(
            focused
                .provider
                .as_ref()
                .map_err(Clone::clone)?
                .processIdentifier(),
            key,
            modifiers,
        )
    }
    fn click_control(
        &self,
        e: &Element,
        window: &Element,
        count: u32,
        token: &tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        self.validate(e, Some(window))?;
        let node = self.inspect(e)?;
        if node.protected || !node.enabled || node.visible == Some(false) {
            return Err("target_unavailable: no click dispatched".into());
        }
        if count == 1 && node.actions.iter().any(|a| a == "AXPress") {
            if token.is_cancelled() {
                return Err("cancelled: no action dispatched".into());
            }
            return self.invoke(e, "AXPress");
        }
        Err("background_pointer_unavailable: this control has no supported single native press action; use an advertised action, or explicitly activate the window and use freshly observed foreground coordinates. No mouse event dispatched.".into())
    }

    fn set_value(&self, e: &Element, value: &str) -> Result<(), String> {
        check(unsafe { AXUIElementSetAttributeValue(e.raw.0, cf("AXValue").0, cf(value).0) })
    }
    fn verify_text_focus(&self, e: &Element, window: &Element) -> Result<(), String> {
        self.validate(e, Some(window))?;
        if self.inspect(e)?.protected {
            return Err("protected: text focus requires user takeover".into());
        }
        let app = owned(unsafe { AXUIElementCreateApplication(e.app.processIdentifier()) })?;
        if !self.has_app_window_focus(window)?
            || !element_attr(app.0, "AXFocusedUIElement")?
                .is_some_and(|v| unsafe { CFEqual(v.0, e.raw.0) })
        {
            return Err("focus_changed: original text control is not the actual application focus; input stopped".into());
        }
        Ok(())
    }
    fn read(&self, e: &Element, offset: usize, limit: usize) -> Result<(String, bool), String> {
        if self.inspect(e)?.protected {
            return Err("protected: user takeover required".into());
        }
        let value = match attr(e.raw.0, "AXValue")? {
            Some(value) => Some(value),
            None => attr(e.raw.0, "AXTitle")?,
        }
        .ok_or("unsupported: provider exposes no readable text")?;
        let (value, capped) = readable(&value, 2_000_000)
            .ok_or("unsupported: provider value type is not readable")?;
        let count = value.chars().count();
        Ok((
            value.chars().skip(offset).take(limit).collect(),
            capped || count > offset.saturating_add(limit),
        ))
    }
}
pub unsafe fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hosted_controls_require_exact_membership_in_the_live_window_tree() {
        let children = |v: &u32| {
            Ok(match v {
                1 => vec![2, 3],
                2 => vec![4],
                _ => vec![],
            })
        };
        assert!(exposed_descendant(1, &4, children, |a, b| a == b).unwrap());
        assert!(!exposed_descendant(1, &8, children, |a, b| a == b).unwrap());
        assert!(!exposed_descendant(1, &8, |_| Ok(vec![1]), |a, b| a == b).unwrap());
    }
    #[test]
    fn nested_save_sheets_are_focus_in_their_original_window() {
        let equal = |a: &u32, b: &u32| a == b;
        let parents = |v: &u32| {
            Ok(match v {
                3 => Some(2),
                2 => Some(1),
                _ => None,
            })
        };
        assert!(window_focus_matches(&3, &1, "AXSheet", Some(1), parents, equal).unwrap());
        assert!(window_focus_matches(&3, &1, "AXSheet", None, parents, equal).unwrap());
        assert!(!window_focus_matches(&3, &9, "AXSheet", Some(1), parents, equal).unwrap());
        assert!(!window_focus_matches(&3, &1, "AXWindow", Some(1), parents, equal).unwrap());
        assert!(!window_focus_matches(&3, &1, "AXApplication", Some(1), parents, equal).unwrap());
        assert!(window_focus_matches(&1, &1, "AXWindow", None, parents, equal).unwrap());
    }
    #[test]
    fn a_provider_disabled_api_is_not_a_revoked_process_permission() {
        assert!(api_disabled(true).starts_with("accessibility_unavailable"));
        assert!(!api_disabled(true).contains("permission_denied"));
        assert!(api_disabled(false).starts_with("permission_denied"));
    }
    #[test]
    fn live_desktop_requires_a_console_login_and_active_display() {
        assert!(check_desktop_state(None, Some(true), Some(true), true).is_ok());
        for state in [
            (Some(true), Some(true), Some(true), true),
            (Some(false), None, Some(true), true),
            (Some(false), Some(true), None, true),
            (Some(false), Some(true), Some(true), false),
        ] {
            assert!(check_desktop_state(state.0, state.1, state.2, state.3)
                .unwrap_err()
                .starts_with("desktop_unavailable"));
        }
    }
    #[test]
    fn applications_or_sheet_controls_cannot_be_exported_as_top_level_windows() {
        assert!(window_role("AXWindow").is_ok());
        for role in ["AXApplication", "AXSheet", "", "AXButton"] {
            assert!(window_role(role).is_err());
        }
    }
    #[test]
    fn unreadable_optional_value_preserves_observation_without_advertising_text() {
        let mut errors = vec![];
        assert!(observed_value(
            Err("native_failure: AX code -25200; read(AXValue)".into()),
            &mut errors
        )
        .unwrap()
        .is_none());
        assert_eq!(errors.len(), 1);
        assert!(observed_value(Err("permission_denied: revoked".into()), &mut errors).is_err());
        assert!(observed_value(Err("native_timeout: provider".into()), &mut errors).is_err());
        assert_eq!(
            text(
                &observed_value(Ok(Some(cf("actual"))), &mut errors)
                    .unwrap()
                    .unwrap(),
                100
            ),
            Some(("actual".into(), false))
        );
    }
    #[test]
    fn sheet_controls_require_a_live_path_to_the_exact_original_window() {
        let equal = |a: &u32, b: &u32| a == b;
        let parents = |id: &u32| {
            Ok(match id {
                3 => Some(2),
                2 => Some(1),
                _ => None,
            })
        };
        assert!(belongs_to_window(&3, &1, None, parents, equal).unwrap());
        assert!(!belongs_to_window(&3, &9, None, parents, equal).unwrap());
        // A changed direct window may not be overruled by a stale parent path.
        assert!(!belongs_to_window(
            &3,
            &1,
            Some(9),
            |_| panic!("must reject direct mismatch"),
            equal
        )
        .unwrap());
        assert!(belongs_to_window(
            &3,
            &1,
            Some(1),
            |_| panic!("direct identity suffices"),
            equal
        )
        .unwrap());
    }
    #[test]
    fn sheet_parent_verification_rejects_cycles_excess_depth_and_provider_errors() {
        let equal = |a: &u32, b: &u32| a == b;
        assert!(belongs_to_window(&3, &1, None, |_| Ok(Some(3)), equal)
            .unwrap_err()
            .contains("cyclic"));
        let mut calls = 0;
        assert!(belongs_to_window(
            &3,
            &1,
            None,
            |id| {
                calls += 1;
                Ok(Some(id + 1))
            },
            equal
        )
        .unwrap_err()
        .contains("limit"));
        assert_eq!(calls, 32);
        assert!(belongs_to_window(
            &3,
            &1,
            None,
            |_| Err("permission_denied: revoked".into()),
            equal
        )
        .unwrap_err()
        .contains("permission_denied"));
    }
    #[test]
    fn failed_description_is_unknown_metadata_and_permission_errors_stay_fatal() {
        let mut errors = vec![];
        let result = metadata_result(
            Err("native_failure: AX code -25200; read(AXDescription)".into()),
            &mut errors,
        )
        .unwrap();
        assert_eq!(result, (String::new(), true));
        assert_eq!(errors.len(), 1);
        assert!(metadata_result(Err("permission_denied: revoked".into()), &mut errors).is_err());
        assert_eq!(
            metadata_result(Ok(Some(cf("合成标签"))), &mut errors).unwrap(),
            ("合成标签".into(), false)
        );
    }
    #[test]
    fn optional_capability_failures_disable_actions_without_hiding_real_errors() {
        let mut errors = vec![];
        assert!(!capability_result(-25200, "settable(AXSelected)", &mut errors).unwrap());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("settable(AXSelected)"));
        assert!(check_at(-25200, "read(AXValue)").is_err());
        assert!(!capability_result(-25205, "settable(AXValue)", &mut errors).unwrap());
        assert!(capability_result(0, "settable(AXValue)", &mut errors).unwrap());
        for code in [-25211, -25202, -25204] {
            assert!(capability_result(code, "settable(AXValue)", &mut errors).is_err());
        }
        assert_eq!(errors.len(), 1);
    }
    #[test]
    fn bounded_native_utf16_does_not_split_emoji() {
        assert_eq!(text(&cf("中😀文"), 2), Some(("中".into(), true)));
        assert_eq!(text(&cf("中😀文"), 4), Some(("中😀文".into(), false)));
    }
    #[test]
    fn native_boolean_and_numeric_values_are_readable_without_becoming_text_fields() {
        let yes = owned(unsafe { CFRetain(kCFBooleanTrue) }).unwrap();
        assert_eq!(readable(&yes, 100), Some(("true".into(), false)));
        assert!(text(&yes, 100).is_none());
        let number = 12.5f64;
        let value =
            owned(unsafe { CFNumberCreate(ptr::null(), 6, &number as *const _ as *const c_void) })
                .unwrap();
        assert_eq!(readable(&value, 100), Some(("12.5".into(), false)));
        assert!(text(&value, 100).is_none());
    }
}
