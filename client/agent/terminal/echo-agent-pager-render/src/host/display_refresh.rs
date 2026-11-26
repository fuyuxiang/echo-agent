//! One-shot primary-display refresh probe (OnceLock-cached).
//! Fail-closed: never panics into callers; no TTY IO; no display mode mutation.

use std::sync::OnceLock;
use std::time::Instant;

use super::{DisplayServer, HostOs, is_wsl};

/// Sane bounds; outside → fail closed.
const MIN_HZ: u32 = 30;
const MAX_HZ: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum DisplayRefreshSource {
    None,
    MacosCoreGraphics,
    WindowsEnumDisplaySettings,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRefreshProbeResult {
    pub hz: Option<u32>,
    pub source: DisplayRefreshSource,
    /// Empty when ok; else a stable skip/error token.
    pub skip_reason: &'static str,
    pub duration_ms: u64,
}

impl DisplayRefreshProbeResult {
    /// `ok` | `skipped` | `error`
    pub fn outcome(self) -> &'static str {
        if self.hz.is_some() {
            "ok"
        } else if self.skip_reason == "error" {
            "error"
        } else {
            "skipped"
        }
    }
}

/// Once per process. Infallible; never panics.
pub fn probe_display_refresh() -> DisplayRefreshProbeResult {
    static CACHE: OnceLock<DisplayRefreshProbeResult> = OnceLock::new();
    *CACHE.get_or_init(probe_uncached)
}

fn probe_uncached() -> DisplayRefreshProbeResult {
    let start = Instant::now();
    let (hz, source, skip_reason) = probe_inner();
    DisplayRefreshProbeResult {
        hz,
        source,
        skip_reason,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

fn probe_inner() -> (Option<u32>, DisplayRefreshSource, &'static str) {
    let is_ssh = echo_agent_shared::clipboard::is_remote_session();
    let wsl = is_wsl();
    let os = HostOs::current();
    let display = DisplayServer::current();

    // Avoid FFI when env already forces a skip.
    let platform_hz = if precheck_skip(is_ssh, wsl).is_some() {
        None
    } else {
        match os {
            HostOs::Macos => Some(probe_macos()),
            HostOs::Windows => Some(probe_windows()),
            HostOs::Linux | HostOs::Other => None,
        }
    };

    decide(is_ssh, wsl, os, display, platform_hz)
}

/// Pure matrix used by production and tests; inject only the platform result.
fn decide(
    is_ssh: bool,
    is_wsl: bool,
    os: HostOs,
    display: DisplayServer,
    platform_hz: Option<Result<u32, &'static str>>,
) -> (Option<u32>, DisplayRefreshSource, &'static str) {
    if let Some(reason) = precheck_skip(is_ssh, is_wsl) {
        return (None, DisplayRefreshSource::None, reason);
    }
    match os {
        HostOs::Macos => {
            let source = DisplayRefreshSource::MacosCoreGraphics;
            match platform_hz.unwrap_or(Err("error")) {
                Ok(hz) => accept_hz(hz, source),
                Err(reason) => (None, source, reason),
            }
        }
        HostOs::Windows => {
            let source = DisplayRefreshSource::WindowsEnumDisplaySettings;
            match platform_hz.unwrap_or(Err("error")) {
                Ok(hz) => accept_hz(hz, source),
                Err(reason) => (None, source, reason),
            }
        }
        HostOs::Linux => {
            let reason = linux_skip_reason(display);
            (None, DisplayRefreshSource::Linux, reason)
        }
        HostOs::Other => (None, DisplayRefreshSource::None, "unsupported"),
    }
}

fn precheck_skip(is_ssh: bool, is_wsl: bool) -> Option<&'static str> {
    if is_ssh {
        return Some("ssh");
    }
    if is_wsl {
        return Some("wsl");
    }
    None
}

fn linux_skip_reason(display: DisplayServer) -> &'static str {
    match display {
        DisplayServer::Wayland => "wayland_unsupported",
        DisplayServer::X11 => "x11_unsupported",
        _ => "no_display",
    }
}

fn accept_hz(
    hz: u32,
    source: DisplayRefreshSource,
) -> (Option<u32>, DisplayRefreshSource, &'static str) {
    if !(MIN_HZ..=MAX_HZ).contains(&hz) {
        return (None, source, "out_of_range");
    }
    (Some(hz), source, "")
}

#[cfg(target_os = "macos")]
fn probe_macos() -> Result<u32, &'static str> {
    // Fail-closed if FFI panics (abort builds still abort).
    match std::panic::catch_unwind(|| {
        // SAFETY: read-only CoreGraphics display query; no mode mutation.
        unsafe { macos_main_display_refresh_hz() }
    }) {
        Ok(inner) => inner,
        Err(_) => Err("error"),
    }
}

#[cfg(not(target_os = "macos"))]
fn probe_macos() -> Result<u32, &'static str> {
    Err("unsupported")
}

#[cfg(target_os = "macos")]
unsafe fn macos_main_display_refresh_hz() -> Result<u32, &'static str> {
    type CgDisplayModeRef = *mut core::ffi::c_void;
    type CgDirectDisplayId = u32;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> CgDirectDisplayId;
        fn CGDisplayCopyDisplayMode(display: CgDirectDisplayId) -> CgDisplayModeRef;
        fn CGDisplayModeGetRefreshRate(mode: CgDisplayModeRef) -> f64;
        fn CGDisplayModeRelease(mode: CgDisplayModeRef);
    }

    // SAFETY: stable public CG APIs; null mode handled; Release pairs with Copy.
    let display = unsafe { CGMainDisplayID() };
    let mode = unsafe { CGDisplayCopyDisplayMode(display) };
    if mode.is_null() {
        return Err("error");
    }
    let rate = unsafe { CGDisplayModeGetRefreshRate(mode) };
    unsafe { CGDisplayModeRelease(mode) };
    // 0.0 is documented indeterminate for some LCD/VRR panels — skip, not error.
    // Future primary-display fallback must be thread-safe; no AppKit/NSScreen here.
    if !rate.is_finite() || rate < 0.0 {
        return Err("error");
    }
    if rate == 0.0 {
        return Err("indeterminate");
    }
    Ok(rate.round() as u32)
}

#[cfg(target_os = "windows")]
fn probe_windows() -> Result<u32, &'static str> {
    match std::panic::catch_unwind(|| {
        // SAFETY: read-only EnumDisplayDevices/Settings for primary only.
        unsafe { windows_primary_display_refresh_hz() }
    }) {
        Ok(inner) => inner,
        Err(_) => Err("error"),
    }
}

#[cfg(not(target_os = "windows"))]
fn probe_windows() -> Result<u32, &'static str> {
    Err("unsupported")
}

/// Primary monitor Hz (matches macOS `CGMainDisplayID`). Null device name to
/// `EnumDisplaySettingsW` is the *current* adapter, which can differ from the
/// primary on multi-monitor machines.
#[cfg(target_os = "windows")]
unsafe fn windows_primary_display_refresh_hz() -> Result<u32, &'static str> {
    use windows_sys::Win32::Graphics::Gdi::{
        DEVMODEW, DISPLAY_DEVICE_PRIMARY_DEVICE, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS,
        EnumDisplayDevicesW, EnumDisplaySettingsW,
    };

    // Bound device enumeration so a broken driver cannot spin forever.
    for i in 0u32..32 {
        // SAFETY: zeroed DISPLAY_DEVICEW with cb set is the documented pattern.
        let mut device: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
        device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        // SAFETY: null parent = desktop adapters; i indexes adapters.
        let ok = unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut device, 0) };
        if ok == 0 {
            break;
        }
        if device.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE == 0 {
            continue;
        }

        // SAFETY: zeroed DEVMODEW with dmSize set; DeviceName is the primary.
        let mut devmode: DEVMODEW = unsafe { std::mem::zeroed() };
        devmode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        let ok = unsafe {
            EnumDisplaySettingsW(
                device.DeviceName.as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut devmode,
            )
        };
        if ok == 0 {
            return Err("error");
        }
        let hz = devmode.dmDisplayFrequency;
        // 0/1 often mean "default hardware rate" — fail closed.
        if hz < 2 {
            return Err("error");
        }
        return Ok(hz);
    }
    Err("error")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real OS path smoke: must not panic (FFI wrapped + fail-closed).
    /// Outcome may be ok/skipped/error depending on host; we only require
    /// process survival and a valid outcome token.
    #[test]
    fn probe_display_refresh_never_panics() {
        let r = probe_display_refresh();
        assert!(
            matches!(r.outcome(), "ok" | "skipped" | "error"),
            "unexpected outcome {:?}",
            r.outcome()
        );
        if let Some(hz) = r.hz {
            assert!((MIN_HZ..=MAX_HZ).contains(&hz), "hz out of bounds: {hz}");
            assert_eq!(r.outcome(), "ok");
            assert!(r.skip_reason.is_empty());
        } else {
            assert!(!r.skip_reason.is_empty() || r.outcome() == "error");
        }
        // Second call hits OnceLock — still must not panic.
        let r2 = probe_display_refresh();
        assert_eq!(r.hz, r2.hz);
        assert_eq!(r.outcome(), r2.outcome());
    }

    #[test]
    fn ssh_skips_before_platform() {
        let (hz, source, reason) = decide(
            true,
            false,
            HostOs::Macos,
            DisplayServer::Unknown,
            Some(Ok(120)),
        );
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::None);
        assert_eq!(reason, "ssh");
    }

    #[test]
    fn wsl_skips() {
        let (hz, source, reason) =
            decide(false, true, HostOs::Linux, DisplayServer::X11, Some(Ok(60)));
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::None);
        assert_eq!(reason, "wsl");
    }

    #[test]
    fn linux_wayland_unsupported() {
        let (hz, source, reason) =
            decide(false, false, HostOs::Linux, DisplayServer::Wayland, None);
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::Linux);
        assert_eq!(reason, "wayland_unsupported");
    }

    #[test]
    fn linux_x11_unsupported() {
        let (hz, source, reason) = decide(false, false, HostOs::Linux, DisplayServer::X11, None);
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::Linux);
        assert_eq!(reason, "x11_unsupported");
    }

    #[test]
    fn linux_no_display() {
        let (hz, source, reason) =
            decide(false, false, HostOs::Linux, DisplayServer::Unknown, None);
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::Linux);
        assert_eq!(reason, "no_display");
    }

    #[test]
    fn host_os_other_unsupported() {
        let (hz, source, reason) =
            decide(false, false, HostOs::Other, DisplayServer::Unknown, None);
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::None);
        assert_eq!(reason, "unsupported");
    }

    #[test]
    fn out_of_range_low() {
        let (hz, source, reason) = decide(
            false,
            false,
            HostOs::Macos,
            DisplayServer::Unknown,
            Some(Ok(15)),
        );
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::MacosCoreGraphics);
        assert_eq!(reason, "out_of_range");
    }

    #[test]
    fn out_of_range_high() {
        let (hz, source, reason) = decide(
            false,
            false,
            HostOs::Windows,
            DisplayServer::Unknown,
            Some(Ok(1000)),
        );
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::WindowsEnumDisplaySettings);
        assert_eq!(reason, "out_of_range");
    }

    #[test]
    fn ok_120_macos() {
        let (hz, source, reason) = decide(
            false,
            false,
            HostOs::Macos,
            DisplayServer::Unknown,
            Some(Ok(120)),
        );
        assert_eq!(hz, Some(120));
        assert_eq!(source, DisplayRefreshSource::MacosCoreGraphics);
        assert_eq!(reason, "");
    }

    #[test]
    fn ok_144_windows() {
        let (hz, source, reason) = decide(
            false,
            false,
            HostOs::Windows,
            DisplayServer::Unknown,
            Some(Ok(144)),
        );
        assert_eq!(hz, Some(144));
        assert_eq!(source, DisplayRefreshSource::WindowsEnumDisplaySettings);
        assert_eq!(reason, "");
    }

    #[test]
    fn backend_error() {
        let (hz, source, reason) = decide(
            false,
            false,
            HostOs::Macos,
            DisplayServer::Unknown,
            Some(Err("error")),
        );
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::MacosCoreGraphics);
        assert_eq!(reason, "error");
    }

    #[test]
    fn indeterminate_is_skipped_not_error() {
        let (hz, source, reason) = decide(
            false,
            false,
            HostOs::Macos,
            DisplayServer::Unknown,
            Some(Err("indeterminate")),
        );
        assert_eq!(
            (hz, source, reason),
            (
                None,
                DisplayRefreshSource::MacosCoreGraphics,
                "indeterminate"
            )
        );
        assert_eq!(
            DisplayRefreshProbeResult {
                hz: None,
                source,
                skip_reason: reason,
                duration_ms: 0,
            }
            .outcome(),
            "skipped"
        );
    }

    #[test]
    fn missing_platform_hz_is_error() {
        let (hz, source, reason) =
            decide(false, false, HostOs::Macos, DisplayServer::Unknown, None);
        assert_eq!(hz, None);
        assert_eq!(source, DisplayRefreshSource::MacosCoreGraphics);
        assert_eq!(reason, "error");
    }

    #[test]
    fn boundary_hz_accepted() {
        for hz_in in [30u32, 500] {
            let (hz, _, reason) = decide(
                false,
                false,
                HostOs::Macos,
                DisplayServer::Unknown,
                Some(Ok(hz_in)),
            );
            assert_eq!(hz, Some(hz_in));
            assert_eq!(reason, "");
        }
    }

    #[test]
    fn source_strum_snake_case_stable() {
        assert_eq!(DisplayRefreshSource::None.as_ref(), "none");
        assert_eq!(
            DisplayRefreshSource::MacosCoreGraphics.as_ref(),
            "macos_core_graphics"
        );
        assert_eq!(
            DisplayRefreshSource::WindowsEnumDisplaySettings.as_ref(),
            "windows_enum_display_settings"
        );
        assert_eq!(DisplayRefreshSource::Linux.as_ref(), "linux");
        assert_eq!(DisplayRefreshSource::Linux.to_string(), "linux");
    }

    #[test]
    fn outcome_tokens() {
        let ok = DisplayRefreshProbeResult {
            hz: Some(120),
            source: DisplayRefreshSource::MacosCoreGraphics,
            skip_reason: "",
            duration_ms: 1,
        };
        assert_eq!(ok.outcome(), "ok");

        let skipped = DisplayRefreshProbeResult {
            hz: None,
            source: DisplayRefreshSource::None,
            skip_reason: "ssh",
            duration_ms: 0,
        };
        assert_eq!(skipped.outcome(), "skipped");

        let err = DisplayRefreshProbeResult {
            hz: None,
            source: DisplayRefreshSource::MacosCoreGraphics,
            skip_reason: "error",
            duration_ms: 2,
        };
        assert_eq!(err.outcome(), "error");
    }
}
