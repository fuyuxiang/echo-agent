#[cfg(windows)]
use tauri::Manager;
#[cfg(not(windows))]
use tauri::PhysicalPosition;
use tauri::{LogicalSize, Monitor, PhysicalSize, WebviewWindow};

// This limit is well beyond desktop display sizes and rejects wrapped Win32
// dimensions even after DPI conversion. The monitor work area sets the actual fit.
const MAX_DIMENSION: f64 = u16::MAX as f64;

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
pub(crate) struct WindowState {
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
    #[serde(default)]
    pub position: Option<(i32, i32)>,
}

impl WindowState {
    pub(crate) fn from_json(payload: &[u8]) -> Option<Self> {
        let state: Self = serde_json::from_slice(payload).ok()?;
        valid_size(state.width, state.height).then_some(state)
    }
}

fn valid_size(width: f64, height: f64) -> bool {
    (1.0..=MAX_DIMENSION).contains(&width) && (1.0..=MAX_DIMENSION).contains(&height)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bounds {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
}

impl Bounds {
    fn work_area(monitor: &Monitor) -> Self {
        let area = monitor.work_area();
        Self {
            x: area.position.x.into(),
            y: area.position.y.into(),
            width: area.size.width.into(),
            height: area.size.height.into(),
        }
    }

    fn contains_origin(self, x: i32, y: i32) -> bool {
        let (x, y) = (i64::from(x), i64::from(y));
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }

    #[cfg(any(windows, test))]
    fn title_reachable(self, area: Self) -> bool {
        let overlap = (self.x + self.width).min(area.x + area.width) - self.x.max(area.x);
        let title_bottom = self.y + self.height.min(32);
        overlap >= self.width.min(64) && title_bottom > area.y && self.y < area.y + area.height
    }

    fn fit(self, position: Option<(i32, i32)>, width: i64, height: i64) -> Self {
        let width = width.clamp(1, self.width);
        let height = height.clamp(1, self.height);
        let (x, y) = position.map_or_else(
            || {
                (
                    self.x + (self.width - width) / 2,
                    self.y + (self.height - height) / 2,
                )
            },
            |(x, y)| {
                (
                    i64::from(x).clamp(self.x, self.x + self.width - width),
                    i64::from(y).clamp(self.y, self.y + self.height - height),
                )
            },
        );
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

pub(crate) fn capture(window: &WebviewWindow) -> Option<WindowState> {
    if window.is_minimized().ok()? || window.is_fullscreen().ok()? {
        return None;
    }
    #[cfg(windows)]
    let (size, position) = {
        let geometry = win32::geometry(window).ok()?;
        if !geometry.reachable(&work_areas(window)) {
            return None;
        }
        (
            PhysicalSize::new(geometry.client.width as f64, geometry.client.height as f64),
            Some((geometry.outer.x as i32, geometry.outer.y as i32)),
        )
    };
    #[cfg(not(windows))]
    let (size, position) = (
        window.inner_size().ok()?.cast::<f64>(),
        window.outer_position().ok().map(|p| (p.x, p.y)),
    );
    let scale = window.scale_factor().ok()?;
    if !scale.is_normal() || scale <= 0.0 {
        return None;
    }
    let size = size.to_logical::<f64>(scale);
    valid_size(size.width, size.height).then_some(WindowState {
        width: size.width,
        height: size.height,
        maximized: window.is_maximized().ok()?,
        position,
    })
}

fn place(window: &WebviewWindow, state: WindowState) -> Result<(), String> {
    let monitors = window.available_monitors().unwrap_or_default();
    let saved_monitor = state.position.and_then(|(x, y)| {
        monitors
            .iter()
            .find(|monitor| Bounds::work_area(monitor).contains_origin(x, y))
    });
    let current = window.current_monitor().ok().flatten();
    let primary = window.primary_monitor().ok().flatten();
    let monitor = saved_monitor
        .or(current.as_ref())
        .or(primary.as_ref())
        .or(monitors.first());
    let position = saved_monitor.and(state.position);
    let work_area = monitor
        .map(Bounds::work_area)
        .filter(|area| area.width > 0 && area.height > 0);
    let scale = monitor
        .map(Monitor::scale_factor)
        .or_else(|| window.scale_factor().ok());
    let scale = scale
        .filter(|scale| scale.is_normal() && *scale > 0.0)
        .unwrap_or(1.0);
    let size: PhysicalSize<u32> = LogicalSize::new(state.width, state.height).to_physical(scale);
    #[cfg(windows)]
    win32::place(window, position, size, work_area)?;
    #[cfg(not(windows))]
    {
        let position_result = if let Some(area) = work_area {
            let fit = area.fit(position, size.width.into(), size.height.into());
            window
                .set_size(PhysicalSize::new(fit.width as u32, fit.height as u32))
                .map_err(|e| e.to_string())?;
            window.set_position(PhysicalPosition::new(fit.x as i32, fit.y as i32))
        } else {
            window.set_size(size).map_err(|e| e.to_string())?;
            window.center()
        };
        if let Err(error) = position_result {
            tracing::debug!(%error, "desktop window position is unavailable");
        }
    }
    if state.maximized {
        window.maximize().map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn restore(window: &WebviewWindow, state: Option<WindowState>) -> Result<(), String> {
    if let Some(state) = state {
        place(window, state)?;
    }
    #[cfg(windows)]
    if window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true) {
        ensure_reachable(window, || None)?;
    }
    Ok(())
}

pub(crate) fn show(
    window: &WebviewWindow,
    saved_state: impl FnOnce() -> Option<WindowState>,
) -> Result<(), String> {
    window.show().map_err(|e| e.to_string())?;
    window.unminimize().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        ensure_reachable(window, saved_state)?;
        if !window.is_visible().map_err(|e| e.to_string())? {
            return Err("main window is still hidden after show".into());
        }
    }
    #[cfg(not(windows))]
    let _ = saved_state;
    if let Err(error) = window.set_focus() {
        tracing::debug!(%error, "could not focus the revealed main window");
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_reachable(
    window: &WebviewWindow,
    saved_state: impl FnOnce() -> Option<WindowState>,
) -> Result<(), String> {
    let geometry = win32::geometry(window)?;
    if geometry.reachable(&work_areas(window)) {
        return Ok(());
    }
    let maximized = window.is_maximized().map_err(|e| e.to_string())?;
    tracing::warn!(outer = ?geometry.outer, client = ?geometry.client, dpi_scale = ?window.scale_factor(), maximized, "repairing unreachable main window geometry");
    let default_state = || {
        let (width, height) = window
            .app_handle()
            .config()
            .app
            .windows
            .iter()
            .find(|config| config.label == window.label())
            .map_or((1280.0, 800.0), |config| (config.width, config.height));
        WindowState {
            width,
            height,
            maximized,
            position: None,
        }
    };
    if maximized {
        window.unmaximize().map_err(|e| e.to_string())?;
    }
    let mut state = saved_state().unwrap_or_else(default_state);
    state.maximized |= maximized;
    place(window, state)?;
    let after = win32::geometry(window)?;
    if !after.reachable(&work_areas(window)) {
        return Err(format!(
            "window geometry remained unreachable after repair: {after:?}"
        ));
    }
    tracing::info!(outer = ?after.outer, client = ?after.client, "repaired main window geometry");
    Ok(())
}

#[cfg(windows)]
fn work_areas(window: &WebviewWindow) -> Vec<Bounds> {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(Bounds::work_area)
        .filter(|area| area.width > 0 && area.height > 0)
        .collect()
}

#[cfg(windows)]
mod win32 {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        UI::WindowsAndMessaging::{
            GetClientRect, GetWindowRect, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
        },
    };

    #[derive(Debug)]
    pub(super) struct Geometry {
        pub outer: Bounds,
        pub client: Bounds,
    }

    impl Geometry {
        pub fn has_area(&self) -> bool {
            self.outer.width > 0
                && self.outer.height > 0
                && self.client.width > 0
                && self.client.height > 0
        }

        pub fn reachable(&self, areas: &[Bounds]) -> bool {
            self.has_area()
                && (areas.is_empty() || areas.iter().any(|area| self.outer.title_reachable(*area)))
        }
    }

    fn bounds(rect: RECT) -> Bounds {
        Bounds {
            x: rect.left.into(),
            y: rect.top.into(),
            width: i64::from(rect.right) - i64::from(rect.left),
            height: i64::from(rect.bottom) - i64::from(rect.top),
        }
    }

    fn hwnd(window: &WebviewWindow) -> Result<HWND, String> {
        window
            .hwnd()
            .map(|handle| handle.0 as HWND)
            .map_err(|e| e.to_string())
    }

    pub(super) fn geometry(window: &WebviewWindow) -> Result<Geometry, String> {
        let handle = hwnd(window)?;
        let mut outer = RECT::default();
        let mut client = RECT::default();
        if unsafe { GetWindowRect(handle, &mut outer) } == 0 {
            return Err(format!(
                "GetWindowRect: {}",
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { GetClientRect(handle, &mut client) } == 0 {
            return Err(format!(
                "GetClientRect: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Geometry {
            outer: bounds(outer),
            client: bounds(client),
        })
    }

    pub(super) fn place(
        window: &WebviewWindow,
        position: Option<(i32, i32)>,
        size: PhysicalSize<u32>,
        area: Option<Bounds>,
    ) -> Result<(), String> {
        let current = geometry(window)?;
        // Persisted dimensions describe the client area. SetWindowPos takes
        // the outer size, including the non-client frame and resize border.
        let width = i64::from(size.width).max(1)
            + (current.outer.width - current.client.width).clamp(0, 512);
        let height = i64::from(size.height).max(1)
            + (current.outer.height - current.client.height).clamp(0, 512);
        let (target, flags) = match area {
            Some(area) => (
                area.fit(position, width, height),
                SWP_NOACTIVATE | SWP_NOZORDER,
            ),
            None => (
                Bounds {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOMOVE,
            ),
        };
        if unsafe {
            SetWindowPos(
                hwnd(window)?,
                std::ptr::null_mut(),
                target.x as i32,
                target.y as i32,
                target.width as i32,
                target.height as i32,
                flags,
            )
        } == 0
        {
            return Err(format!("SetWindowPos: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_legacy_dimensions_are_ignored_but_large_and_old_windows_are_valid() {
        assert!(WindowState::from_json(br#"{"width":2863311516.0,"height":2863311522.0,"maximized":false,"position":[0,0]}"#).is_none());
        let old =
            WindowState::from_json(br#"{"width":7680,"height":4320,"maximized":false}"#).unwrap();
        assert_eq!(old.width, 7680.0);
        assert!(old.position.is_none());
        assert!(!valid_size(f64::NAN, 800.0));
        assert!(!valid_size(1280.0, 0.0));
    }

    #[test]
    fn fits_inside_the_target_work_area_and_preserves_a_valid_position() {
        let area = Bounds {
            x: -2560,
            y: 40,
            width: 2560,
            height: 1400,
        };
        assert_eq!(
            area.fit(Some((-2400, 60)), 1600, 900),
            Bounds {
                x: -2400,
                y: 60,
                width: 1600,
                height: 900
            }
        );
        assert_eq!(area.fit(Some((2000, -500)), 3000, 1800), area);
        let window = area.fit(None, 1280, 800);
        assert_eq!((window.x, window.y), (-1920, 340));
        assert!(window.title_reachable(area));
        assert!(!Bounds { y: -100, ..window }.title_reachable(area));
        assert!(!Bounds { x: 4000, ..window }.title_reachable(area));
    }
}
