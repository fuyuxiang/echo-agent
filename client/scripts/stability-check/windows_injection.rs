//! Fixed Windows fault injection, compiled even by the cross-platform API check.
//! Only the isolated fixture calls it; targets are verified against its native
//! WebView environment, host ancestry and an open handle's creation timestamp.
use crate::process_identity;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Environment8, ICoreWebView2_2, COREWEBVIEW2_PROCESS_KIND_RENDERER,
};
use windows_core::Interface;
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME},
    System::Threading::{
        GetProcessTimes, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    },
};

pub fn terminate_owned_webview(core: &ICoreWebView2, renderer: bool) -> Result<(), String> {
    let mut browser_pid = 0;
    unsafe { core.BrowserProcessId(&mut browser_pid) }.map_err(|e| e.to_string())?;
    let mut target = browser_pid;
    if renderer {
        let environment: ICoreWebView2Environment8 = unsafe {
            core.cast::<ICoreWebView2_2>()
                .map_err(|e| e.to_string())?
                .Environment()
        }
        .map_err(|e| e.to_string())?
        .cast()
        .map_err(|e| e.to_string())?;
        let infos = unsafe { environment.GetProcessInfos() }.map_err(|e| e.to_string())?;
        let mut count = 0;
        unsafe { infos.Count(&mut count) }.map_err(|e| e.to_string())?;
        target = 0;
        for index in 0..count {
            let info = unsafe { infos.GetValueAtIndex(index) }.map_err(|e| e.to_string())?;
            let mut process_kind = COREWEBVIEW2_PROCESS_KIND_RENDERER;
            let mut pid = 0;
            unsafe { info.Kind(&mut process_kind) }.map_err(|e| e.to_string())?;
            unsafe { info.ProcessId(&mut pid) }.map_err(|e| e.to_string())?;
            if process_kind == COREWEBVIEW2_PROCESS_KIND_RENDERER && pid > 0 {
                target = pid as u32;
                break;
            }
        }
    }
    let inventory = process_identity::snapshot()?;
    let mut parents = std::collections::HashSet::from([std::process::id()]);
    for _ in 0..inventory.len() {
        let before = parents.len();
        for process in &inventory {
            if parents.contains(&process.parent) {
                parents.insert(process.pid);
            }
        }
        if before == parents.len() {
            break;
        }
    }
    if target == 0 || target == std::process::id() || !parents.contains(&target) {
        return Err("refused non-owned failure injection".into());
    }
    let expected = inventory
        .iter()
        .find(|p| p.pid == target)
        .ok_or("target disappeared")?;
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            target,
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    let identified =
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } != 0
            && ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
                .to_string()
                == expected.started;
    if !identified {
        unsafe { CloseHandle(handle) };
        return Err("target identity changed".into());
    }
    let result = unsafe { TerminateProcess(handle, 91) };
    unsafe { CloseHandle(handle) };
    if result == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}
