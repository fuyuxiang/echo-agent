//! All COM interfaces and process handles stay on the session's MTA worker.
use super::{Listing, Native, Node};
use std::{collections::HashSet, rc::Rc, time::Instant};
use windows::{
    core::{Interface, BSTR, PCWSTR, PWSTR},
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            Threading::{
                OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
                SYNCHRONIZATION_SYNCHRONIZE,
            },
        },
        UI::{
            Accessibility::*,
            WindowsAndMessaging::{
                GetForegroundWindow, SetForegroundWindow, ShowWindow, SW_RESTORE,
            },
        },
    },
};
fn bounded(value: &BSTR, max: usize) -> (String, bool) {
    (
        String::from_utf16_lossy(&value[..value.len().min(max)]),
        value.len() > max,
    )
}
fn optional_element(
    result: windows::core::Result<IUIAutomationElement>,
) -> Result<Option<IUIAutomationElement>, String> {
    match result {
        Ok(element) => Ok(Some(element)),
        Err(e) if e.code().0 as u32 == 0x80004003 => Ok(None),
        Err(e) => Err(error(e)),
    }
}
fn error(e: windows::core::Error) -> String {
    let category = match e.code().0 as u32 {
        0x80070005 => "permission_denied",
        0x80040201 => "stale_reference",
        0x80131505 | 0x800705B4 => "native_timeout",
        _ => "native_failure",
    };
    format!(
        "{category}: UI Automation HRESULT {} (check provider, elevation and interactive desktop)",
        e.code()
    )
}
// Version.dll follows the installed MUI resources for the user's language.
// Failure is optional metadata: keep executable names and native identity.
pub(super) fn application_descriptions(path: &str) -> Vec<String> {
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoExW, GetFileVersionInfoSizeExW, VerQueryValueW, FILE_VER_GET_LOCALISED,
    };
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let mut handle = 0;
        let size =
            GetFileVersionInfoSizeExW(FILE_VER_GET_LOCALISED, PCWSTR(wide.as_ptr()), &mut handle);
        if size == 0 || size > 1024 * 1024 {
            return vec![];
        }
        // Aligned storage for the native version structures.
        let mut data = vec![0u32; (size as usize).div_ceil(4)];
        if GetFileVersionInfoExW(
            FILE_VER_GET_LOCALISED,
            PCWSTR(wide.as_ptr()),
            None,
            size,
            data.as_mut_ptr().cast(),
        )
        .is_err()
        {
            return vec![];
        }
        let query = |key: &str| -> Option<(*const u16, usize)> {
            let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0);
            if !VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(key.as_ptr()),
                &mut ptr,
                &mut len,
            )
            .as_bool()
                || ptr.is_null()
            {
                return None;
            }
            Some((ptr.cast(), len as usize))
        };
        let Some((translations, bytes)) = query(r"\VarFileInfo\Translation") else {
            return vec![];
        };
        let within = |ptr: *const u16, bytes: usize| {
            let start = data.as_ptr() as usize;
            let end = start + size as usize;
            let address = ptr as usize;
            address >= start && address.checked_add(bytes).is_some_and(|limit| limit <= end)
        };
        if !within(translations, bytes) {
            return vec![];
        }
        let translations = std::slice::from_raw_parts(translations, (bytes / 2).min(64));
        let mut names = Vec::new();
        for pair in translations.as_chunks::<2>().0 {
            let key = format!(
                r"\StringFileInfo\{:04x}{:04x}\FileDescription",
                pair[0], pair[1]
            );
            if let Some((text, units)) = query(&key) {
                if units > 1025 {
                    continue;
                }
                if !within(text, units * 2) {
                    continue;
                }
                let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, units))
                    .trim_end_matches('\0')
                    .trim()
                    .to_owned();
                if !value.is_empty() && !names.contains(&value) {
                    names.push(value);
                }
            }
        }
        names
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}
struct Process(HANDLE);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
#[derive(Clone)]
pub struct Element {
    ui: IUIAutomationElement,
    process: Rc<Process>,
    pid: i32,
}
type WindowEnumeration = (Vec<Element>, Vec<(String, String)>);

pub struct Backend {
    ui: IUIAutomation,
    _apartment: Apartment,
}
impl Backend {
    fn wrap(&self, ui: IUIAutomationElement) -> Result<Element, String> {
        let pid = unsafe { ui.CurrentProcessId() }.map_err(error)?;
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION
                    | windows::Win32::System::Threading::PROCESS_ACCESS_RIGHTS(
                        SYNCHRONIZATION_SYNCHRONIZE.0,
                    ),
                false,
                pid as u32,
            )
        }
        .map_err(error)?;
        Ok(Element {
            ui,
            process: Rc::new(Process(process)),
            pid,
        })
    }
    fn value(&self, e: &Element, max: i32) -> Result<(Option<String>, bool), String> {
        unsafe {
            if e.ui.CurrentIsPassword().map_err(error)?.as_bool() {
                return Ok((None, false));
            }
            if let Ok(p) =
                e.ui.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            {
                let (value, truncated) = bounded(&p.CurrentValue().map_err(error)?, max as usize);
                return Ok((Some(value), truncated));
            }
            if let Ok(p) =
                e.ui.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            {
                let s = p
                    .DocumentRange()
                    .map_err(error)?
                    .GetText(max + 1)
                    .map_err(error)?
                    .to_string();
                let truncated = s.encode_utf16().count() > max as usize;
                return Ok((Some(s.chars().take(max as usize).collect()), truncated));
            }
            Ok((None, false))
        }
    }
    fn top_windows(&self, pid: Option<i32>) -> Result<WindowEnumeration, String> {
        unsafe {
            let root = self.ui.GetRootElement().map_err(error)?;
            let walker = self.ui.ControlViewWalker().map_err(error)?;
            let mut current = optional_element(walker.GetFirstChildElement(&root))?;
            let mut output = vec![];
            let mut issues = vec![];
            let mut scanned = 0;
            let start = Instant::now();
            while let Some(ui) = current {
                if scanned >= 256 || start.elapsed() > super::BUDGET {
                    issues.push((
                        "desktop".into(),
                        "discovery_limit: window enumeration bounded; retry with appRef".into(),
                    ));
                    break;
                }
                scanned += 1;
                let next = match optional_element(walker.GetNextSiblingElement(&ui)) {
                    Ok(next) => next,
                    Err(error) => {
                        issues.push(("desktop".into(), error));
                        None
                    }
                };
                match ui.CurrentProcessId().map_err(error) {
                    Ok(found) if pid.is_none_or(|expected| expected == found) => {
                        match self.wrap(ui) {
                            Ok(element) => output.push(element),
                            Err(error) => issues.push((format!("process:{found}"), error)),
                        }
                    }
                    Ok(_) => {}
                    Err(error) => issues.push(("unknown process".into(), error)),
                }
                current = next;
            }
            Ok((output, issues))
        }
    }
}
impl Native for Backend {
    fn hit_target(&self, window: &Element, x: f64, y: f64) -> Result<Element, String> {
        let hit = self.wrap(
            unsafe {
                self.ui.ElementFromPoint(windows::Win32::Foundation::POINT {
                    x: x.round() as i32,
                    y: y.round() as i32,
                })
            }
            .map_err(error)?,
        )?;
        self.validate(&hit, Some(window))?;
        Ok(hit)
    }

    fn commands(&self, e: &Element) -> Result<Option<Vec<serde_json::Value>>, String> {
        let mut pending = std::collections::VecDeque::from([e.clone()]);
        let mut result = vec![];
        let start = Instant::now();
        let mut count = 0;
        while let Some(item) = pending.pop_front() {
            count += 1;
            if count > 450 || start.elapsed() > std::time::Duration::from_millis(700) {
                break;
            }
            unsafe {
                if item.ui.CurrentControlType().map_err(error)? == UIA_MenuItemControlTypeId {
                    let shortcut = item.ui.CurrentAcceleratorKey().map_err(error)?.to_string();
                    if !shortcut.is_empty() {
                        result.push(serde_json::json!({"label":item.ui.CurrentName().map_err(error)?.to_string(),"shortcut":shortcut.to_lowercase().replace("control+","ctrl+"),"enabled":item.ui.CurrentIsEnabled().map_err(error)?.as_bool()}));
                    }
                }
            }
            if let Ok((children, _)) = self.children(&item, 64) {
                pending.extend(children);
            }
        }
        Ok(Some(result))
    }
    fn file_dialog(
        &self,
        window: &Element,
    ) -> Result<Option<super::file_dialog::Dialog<Element>>, String> {
        self.validate(window, None)?;
        let class = unsafe { window.ui.CurrentClassName() }
            .map_err(error)?
            .to_string();
        if class != "#32770" {
            return Ok(None);
        }
        let mut pending = std::collections::VecDeque::from([window.clone()]);
        let mut field = None;
        let mut save = false;
        let mut count = 0;
        while let Some(item) = pending.pop_front() {
            count += 1;
            if count > 200 {
                break;
            }
            let node = self.inspect(&item)?;
            if node.role == "edit" && ["1148", "1001"].contains(&node.identifier.as_str()) {
                field = Some(item.clone());
            }
            if node.identifier == "1"
                && ["save", "保存", "保存(&s)"].contains(&node.name.to_lowercase().as_str())
            {
                save = true;
            }
            if let Ok((children, _)) = self.children(&item, 64) {
                pending.extend(children);
            }
        }
        Ok(field.map(|filename| super::file_dialog::Dialog {
            kind: if save { "save" } else { "open" },
            root: window.clone(),
            path: None,
            filename: Some(filename),
        }))
    }
    fn click_control(
        &self,
        e: &Element,
        window: &Element,
        count: u32,
        token: &tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        self.validate(e, Some(window))?;
        if !self.activate(window)? {
            return Err("focus_changed: no click dispatched".into());
        }
        let node = self.inspect(e)?;
        if node.protected || !node.enabled || node.visible == Some(false) {
            return Err("target_unavailable".into());
        }
        let [x, y, width, height] = node.bounds.ok_or("bounds_unavailable")?;
        if width <= 0.0 || height <= 0.0 {
            return Err("bounds_unavailable".into());
        }
        let (x, y) = (x + width / 2.0, y + height / 2.0);
        unsafe {
            let hit = self.wrap(
                self.ui
                    .ElementFromPoint(windows::Win32::Foundation::POINT {
                        x: x.round() as i32,
                        y: y.round() as i32,
                    })
                    .map_err(error)?,
            )?;
            self.validate(&hit, Some(e))?;
            if self.inspect(&hit)?.protected {
                return Err("protected: user takeover required".into());
            }
        }
        if token.is_cancelled() {
            return Err("cancelled: no click dispatched".into());
        }
        super::super::computer::click_after_native_target(x, y, count)
    }
    fn foreground_window(&self) -> Result<Element, String> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return Err("focus_unavailable: no foreground window".into());
            }
            self.wrap(self.ui.ElementFromHandle(hwnd).map_err(error)?)
        }
    }
    fn keyboard_focus(&self, window: &Element) -> Result<Option<Element>, String> {
        let focus = unsafe { self.ui.GetFocusedElement() };
        let focus = match focus {
            Ok(focus) => focus,
            Err(e) if e.code().0 as u32 == 0x80070005 => return Err(error(e)),
            Err(_) => return Ok(None),
        };
        let focus = self.wrap(focus)?;
        self.validate(&focus, Some(window))?;
        Ok(Some(focus))
    }
    fn focused_target(&self) -> Result<(Element, Element), String> {
        let window = self.foreground_window()?;
        let focus = self
            .keyboard_focus(&window)?
            .ok_or("focus_unavailable: no control")?;
        Ok((window, focus))
    }

    fn same_element(&self, a: &Element, b: &Element) -> bool {
        unsafe {
            self.ui
                .CompareElements(&a.ui, &b.ui)
                .map(|v| v.as_bool())
                .unwrap_or(false)
        }
    }
    fn application_element(&self, e: &Element) -> Result<Option<Element>, String> {
        self.validate(e, None)?;
        Ok(Some(e.clone()))
    }
    type Element = Element;
    fn new() -> Result<Self, String> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(error)?;
            let apartment = Apartment;
            let advanced: IUIAutomation2 =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).map_err(error)?;
            advanced.SetConnectionTimeout(250).map_err(error)?;
            advanced.SetTransactionTimeout(500).map_err(error)?;
            let ui: IUIAutomation = advanced.cast().map_err(error)?;
            // Probe actual desktop access, never infer availability from OS alone.
            ui.GetRootElement()
                .map_err(error)?
                .CurrentProcessId()
                .map_err(error)?;
            Ok(Self {
                ui,
                _apartment: apartment,
            })
        }
    }
    fn open_app(&self, path: &str) -> Result<bool, String> {
        let child = std::process::Command::new(path)
            .current_dir(
                std::path::Path::new(path)
                    .parent()
                    .ok_or("invalid appPath")?,
            )
            .spawn()
            .map_err(|e| format!("launch_failed: {e}"))?;
        let pid = child.id() as i32;
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.top_windows(Some(pid))?.0.iter().any(|e| e.pid == pid) {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(false)
    }
    fn apps(&self) -> Result<Listing<Element>, String> {
        let start = Instant::now();
        let mut seen = HashSet::new();
        let (windows, issues) = self.top_windows(None)?;
        let mut output = Listing {
            items: vec![],
            issues,
        };
        for e in windows {
            if start.elapsed() > super::BUDGET {
                output.issues.push((String::new(), "native_timeout: application inspection budget exceeded; discovery is incomplete".into()));
                break;
            }
            if !seen.contains(&e.pid) {
                match self.inspect(&e) {
                    Ok(mut n) => {
                        seen.insert(e.pid);
                        n.role = "application".into();
                        n.identifier = format!("process:{}", e.pid);
                        let descriptions = application_descriptions(&n.application);
                        (n.name, n.aliases) =
                            super::app_names::windows_names(&n.application, &descriptions);
                        output.items.push((e, n));
                    }
                    Err(error) => output.issues.push((format!("process:{}", e.pid), error)),
                }
            }
        }
        Ok(output)
    }
    fn windows(&self, app: Option<&Element>) -> Result<Listing<Element>, String> {
        let start = Instant::now();
        if let Some(app) = app {
            self.validate(app, None)?;
        }
        let (windows, issues) = self.top_windows(app.map(|a| a.pid))?;
        let mut output = Listing {
            items: vec![],
            issues,
        };
        for element in windows {
            if start.elapsed() > super::BUDGET {
                output.issues.push((
                    String::new(),
                    "native_timeout: window inspection budget exceeded; retry with appRef".into(),
                ));
                break;
            }
            match self.inspect(&element) {
                Ok(node) => output.items.push((element, node)),
                Err(error) => output
                    .issues
                    .push((format!("process:{}", element.pid), error)),
            }
        }
        if let Some(app) = app {
            self.validate(app, None)?;
        }
        Ok(output)
    }
    fn process_id(&self, e: &Element) -> Option<u32> {
        u32::try_from(e.pid).ok()
    }
    #[allow(non_upper_case_globals)]
    fn inspect(&self, e: &Element) -> Result<Node, String> {
        unsafe {
            self.validate(e, None)?;
            let protected = e.ui.CurrentIsPassword().map_err(error)?.as_bool();
            let role = e.ui.CurrentControlType().map_err(error)?;
            let role = match role {
                UIA_ButtonControlTypeId => "button",
                UIA_EditControlTypeId => "edit",
                UIA_DocumentControlTypeId => "document",
                UIA_WindowControlTypeId => "window",
                UIA_TextControlTypeId => "text",
                UIA_ListItemControlTypeId => "list_item",
                UIA_ComboBoxControlTypeId => "combo_box",
                UIA_ListControlTypeId => "list",
                UIA_TreeControlTypeId => "tree",
                UIA_TreeItemControlTypeId => "tree_item",
                UIA_MenuControlTypeId => "menu",
                UIA_MenuItemControlTypeId => "menu_item",
                UIA_CheckBoxControlTypeId => "check_box",
                UIA_RadioButtonControlTypeId => "radio_button",
                UIA_TabControlTypeId => "tab",
                UIA_TabItemControlTypeId => "tab_item",
                UIA_TableControlTypeId => "table",
                UIA_DataGridControlTypeId => "data_grid",
                UIA_DataItemControlTypeId => "data_item",
                UIA_PaneControlTypeId => "pane",
                UIA_GroupControlTypeId => "group",
                UIA_ImageControlTypeId => "image",
                UIA_ScrollBarControlTypeId => "scroll_bar",
                UIA_SliderControlTypeId => "slider",
                UIA_HyperlinkControlTypeId => "hyperlink",
                _ => "control",
            }
            .to_string();
            let mut actions = vec![];
            if e.ui.CurrentIsKeyboardFocusable().map_err(error)?.as_bool() {
                actions.push("focus".into());
            }
            if e.ui
                .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                .is_ok()
            {
                actions.push("invoke".into());
            }
            if e.ui
                .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                    UIA_SelectionItemPatternId,
                )
                .is_ok()
            {
                actions.push("select".into());
            }
            if e.ui
                .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                    UIA_ExpandCollapsePatternId,
                )
                .is_ok()
            {
                actions.extend(["expand".into(), "collapse".into()]);
            }
            if e.ui
                .GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
                .is_ok()
            {
                actions.extend(["scroll_down".into(), "scroll_up".into()]);
            }
            if e.ui
                .GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)
                .is_ok()
            {
                actions.push("scroll_into_view".into());
            }
            let writable =
                e.ui.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    .ok()
                    .and_then(|p| p.CurrentIsReadOnly().ok())
                    .is_some_and(|r| !r.as_bool());
            let bounds = e.ui.CurrentBoundingRectangle().map_err(error)?;
            let (value, text_truncated) = self.value(e, 4096)?;
            let hwnd = e.ui.CurrentNativeWindowHandle().map_err(error)?;
            let mut image = vec![0u16; 32768];
            let mut image_len = image.len() as u32;
            let application = if windows::Win32::System::Threading::QueryFullProcessImageNameW(
                e.process.0,
                windows::Win32::System::Threading::PROCESS_NAME_WIN32,
                PWSTR(image.as_mut_ptr()),
                &mut image_len,
            )
            .is_ok()
            {
                String::from_utf16_lossy(&image[..image_len as usize])
            } else {
                format!("process:{}", e.pid)
            };
            let (name, name_truncated) = if protected {
                ("[protected]".into(), false)
            } else {
                bounded(&e.ui.CurrentName().map_err(error)?, 1024)
            };
            let (identifier, identifier_truncated) =
                bounded(&e.ui.CurrentAutomationId().map_err(error)?, 1024);
            Ok(Node {
                aliases: vec![],
                application,
                role,
                name,
                identifier,
                value,
                enabled: e.ui.CurrentIsEnabled().map_err(error)?.as_bool(),
                visible: Some(!e.ui.CurrentIsOffscreen().map_err(error)?.as_bool()),
                focused: e.ui.CurrentHasKeyboardFocus().map_err(error)?.as_bool()
                    || (!hwnd.0.is_null() && GetForegroundWindow() == hwnd),
                selected: e
                    .ui
                    .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                    .ok()
                    .map(|p| p.CurrentIsSelected().map(|v| v.as_bool()))
                    .transpose()
                    .map_err(error)?,
                expanded: e
                    .ui
                    .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                    .ok()
                    .map(|p| {
                        p.CurrentExpandCollapseState()
                            .map(|s| s == ExpandCollapseState_Expanded)
                    })
                    .transpose()
                    .map_err(error)?,
                protected,
                writable: writable && !protected,
                bounds: Some([
                    bounds.left as f64,
                    bounds.top as f64,
                    (bounds.right - bounds.left) as f64,
                    (bounds.bottom - bounds.top) as f64,
                ]),
                actions: if protected { vec![] } else { actions },
                text_truncated,
                metadata_truncated: name_truncated || identifier_truncated,
                observation_errors: vec![],
            })
        }
    }
    fn children(&self, e: &Element, limit: usize) -> Result<(Vec<Element>, bool), String> {
        unsafe {
            let walker = self.ui.ControlViewWalker().map_err(error)?;
            let mut next = optional_element(walker.GetFirstChildElement(&e.ui))?;
            let mut output = vec![];
            let start = Instant::now();
            while let Some(child) = next {
                if output.len() >= limit || start.elapsed() > super::BUDGET {
                    return Ok((output, true));
                }
                next = optional_element(walker.GetNextSiblingElement(&child))?;
                output.push(self.wrap(child)?);
            }
            Ok((output, false))
        }
    }
    fn validate(&self, e: &Element, window: Option<&Element>) -> Result<(), String> {
        unsafe {
            if WaitForSingleObject(e.process.0, 0).0 != 258 {
                return Err("application_exited: process exited or inaccessible".into());
            }
            if e.ui.CurrentProcessId().map_err(error)? != e.pid {
                return Err("stale_reference: UIA process identity changed".into());
            }
            if let Some(window) = window {
                let walker = self.ui.RawViewWalker().map_err(error)?;
                let mut current = Some(e.ui.clone());
                for _ in 0..64 {
                    let Some(item) = current else {
                        break;
                    };
                    if self
                        .ui
                        .CompareElements(&item, &window.ui)
                        .map_err(error)?
                        .as_bool()
                    {
                        return Ok(());
                    }
                    current = optional_element(walker.GetParentElement(&item))?;
                }
                return Err("stale_reference: control detached from original window".into());
            }
            Ok(())
        }
    }
    fn activate(&self, e: &Element) -> Result<bool, String> {
        unsafe {
            let hwnd: HWND = e.ui.CurrentNativeWindowHandle().map_err(error)?;
            if hwnd.0.is_null() {
                return Err("unsupported: control has no top-level window handle".into());
            }
            let _ = ShowWindow(hwnd, SW_RESTORE);
            if !SetForegroundWindow(hwnd).as_bool() {
                return Err(
                    "activation_denied: Windows foreground restrictions or privilege mismatch"
                        .into(),
                );
            }
            let start = Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(1) {
                if GetForegroundWindow() == hwnd {
                    return Ok(true);
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            Ok(false)
        }
    }
    fn invoke(&self, e: &Element, action: &str) -> Result<(), String> {
        unsafe {
            match action {
                "focus" => e.ui.SetFocus(),
                "invoke" => {
                    e.ui.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                        .map_err(error)?
                        .Invoke()
                }
                "select" => {
                    e.ui.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                    .map_err(error)?
                    .Select()
                }
                "expand" => {
                    e.ui.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                    .map_err(error)?
                    .Expand()
                }
                "collapse" => {
                    e.ui.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                    .map_err(error)?
                    .Collapse()
                }
                "scroll_down" | "scroll_up" => {
                    e.ui.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
                        .map_err(error)?
                        .Scroll(
                            ScrollAmount_NoAmount,
                            if action == "scroll_down" {
                                ScrollAmount_LargeIncrement
                            } else {
                                ScrollAmount_LargeDecrement
                            },
                        )
                }
                "scroll_into_view" => e
                    .ui
                    .GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)
                    .map_err(error)?
                    .ScrollIntoView(),
                _ => return Err("unsupported_action".into()),
            }
            .map_err(error)
        }
    }
    fn key(&self, e: &Element, key: &str, modifiers: &[String]) -> Result<(), String> {
        unsafe {
            self.validate(e, None)?;
            let focused = self.wrap(self.ui.GetFocusedElement().map_err(error)?)?;
            self.validate(&focused, Some(e))?;
            if focused.ui.CurrentIsPassword().map_err(error)?.as_bool() {
                return Err("protected: keyboard focus requires user takeover".into());
            }
            if GetForegroundWindow() != e.ui.CurrentNativeWindowHandle().map_err(error)? {
                return Err(
                    "focus_changed: approved window is no longer foreground; key not dispatched"
                        .into(),
                );
            }
        }
        super::super::computer::key_after_native_focus(key, modifiers)
    }
    fn set_value(&self, e: &Element, value: &str) -> Result<(), String> {
        unsafe {
            e.ui.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                .map_err(error)?
                .SetValue(&BSTR::from(value))
                .map_err(error)
        }
    }
    fn verify_text_focus(&self, e: &Element, window: &Element) -> Result<(), String> {
        unsafe {
            self.validate(e, Some(window))?;
            let focused = self.ui.GetFocusedElement().map_err(error)?;
            if focused.CurrentIsPassword().map_err(error)?.as_bool() {
                return Err("protected: text focus requires user takeover".into());
            }
            if !self
                .ui
                .CompareElements(&focused, &e.ui)
                .map_err(error)?
                .as_bool()
                || GetForegroundWindow() != window.ui.CurrentNativeWindowHandle().map_err(error)?
            {
                return Err("focus_changed: original text control is not the actual foreground focus; input stopped".into());
            }
            Ok(())
        }
    }
    fn read(&self, e: &Element, offset: usize, limit: usize) -> Result<(String, bool), String> {
        if self.inspect(e)?.protected {
            return Err("protected: user takeover required".into());
        }
        let (value, mut capped) = self.value(e, 2_000_000)?;
        let value = match value {
            Some(value) => value,
            None => {
                let (value, more) =
                    bounded(&unsafe { e.ui.CurrentName() }.map_err(error)?, 2_000_000);
                capped |= more;
                value
            }
        };
        let count = value.chars().count();
        Ok((
            value.chars().skip(offset).take(limit).collect(),
            capped || count > offset.saturating_add(limit),
        ))
    }
}
