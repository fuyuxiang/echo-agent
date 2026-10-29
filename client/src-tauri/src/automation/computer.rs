use base64::Engine;
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde::Serialize;
use std::io::Cursor;
use std::time::Duration;

const MAX_SCREENSHOT_WIDTH: u32 = 1920;
const MAX_SCREENSHOT_HEIGHT: u32 = 1200;
const MAX_FRAME_AGE_MS: u64 = 30 * 1_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerCapability {
    pub available: bool,
    pub desktop_available: Option<bool>,
    pub platform: String,
    pub accessibility: super::accessibility::Capability,
    pub screen_capture: bool,
    pub input_control: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub primary: bool,
    pub origin_x: f64,
    pub origin_y: f64,
    pub logical_width: f64,
    pub logical_height: f64,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub scale_factor: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMeta {
    pub frame_id: String,
    pub display_id: String,
    pub image_width: u32,
    pub image_height: u32,
    pub origin_x: f64,
    pub origin_y: f64,
    pub logical_width: f64,
    pub logical_height: f64,
    pub scale_factor: f64,
    pub captured_at_ms: u64,
    pub target_name: Option<String>,
}

#[derive(Debug)]
pub struct ScreenshotFrame {
    pub png_base64: String,
    pub meta: FrameMeta,
}

#[derive(Debug, Default)]
pub struct ComputerController {
    last_frame: Option<StoredFrame>,
}

// Crate-internal dispatch used only by the native worker after its retained
// window is approved, activated, and its foreground/protected focus rechecked.
// This is not an IPC/MCP entry point; legacy frame-based key() is unchanged.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) fn key_after_native_focus(key: &str, modifiers: &[String]) -> Result<(), String> {
    platform::key(key, modifiers)
}

#[cfg(target_os = "macos")]
pub(super) fn click_after_native_target(x: f64, y: f64, count: u32) -> Result<(), String> {
    platform::click(x, y, "left", count)
}

pub(super) fn text_after_native_focus(text: &str) -> Result<(), String> {
    platform::type_text(text)
}

#[derive(Debug)]
struct StoredFrame {
    meta: FrameMeta,
    visual_signature: Vec<u8>,
    foreground_target: Option<ForegroundTarget>,
}

#[derive(Debug, Clone)]
struct ForegroundTarget {
    id: String,
    name: String,
}

impl ComputerController {
    pub fn capability() -> ComputerCapability {
        platform::capability()
    }

    pub fn request_permissions() -> Result<ComputerCapability, String> {
        platform::request_permissions()?;
        Ok(platform::capability())
    }

    pub fn displays() -> Result<Vec<DisplayInfo>, String> {
        platform::displays()
    }

    pub fn screenshot(&mut self, display_id: Option<&str>) -> Result<ScreenshotFrame, String> {
        let foreground_target = platform::foreground_target();
        let captured = platform::capture(display_id)?;
        if captured.width == 0 || captured.height == 0 {
            return Err("屏幕截图尺寸无效".into());
        }
        let image = RgbaImage::from_raw(captured.width, captured.height, captured.rgba)
            .ok_or_else(|| "屏幕截图像素数据不完整".to_string())?;
        let mut dynamic = DynamicImage::ImageRgba8(image);
        if dynamic.width() > MAX_SCREENSHOT_WIDTH || dynamic.height() > MAX_SCREENSHOT_HEIGHT {
            dynamic = dynamic.resize(
                MAX_SCREENSHOT_WIDTH,
                MAX_SCREENSHOT_HEIGHT,
                image::imageops::FilterType::Triangle,
            );
        }
        let image_width = dynamic.width();
        let image_height = dynamic.height();
        let visual_signature = visual_signature(&dynamic);
        let mut png = Cursor::new(Vec::new());
        dynamic
            .write_to(&mut png, ImageFormat::Png)
            .map_err(|error| format!("编码屏幕截图失败：{error}"))?;
        let scale_factor = if captured.logical_width > 0.0 {
            f64::from(image_width) / captured.logical_width
        } else {
            1.0
        };
        let meta = FrameMeta {
            frame_id: uuid::Uuid::now_v7().to_string(),
            display_id: captured.display_id,
            image_width,
            image_height,
            origin_x: captured.origin_x,
            origin_y: captured.origin_y,
            logical_width: captured.logical_width,
            logical_height: captured.logical_height,
            scale_factor,
            captured_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            target_name: foreground_target.as_ref().map(|target| target.name.clone()),
        };
        self.last_frame = Some(StoredFrame {
            meta: meta.clone(),
            visual_signature,
            foreground_target,
        });
        Ok(ScreenshotFrame {
            png_base64: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
            meta,
        })
    }

    pub fn move_pointer(&mut self, frame_id: &str, x: f64, y: f64) -> Result<(f64, f64), String> {
        self.verify_frame_unchanged(frame_id)?;
        let (desktop_x, desktop_y) = self.resolve_point(frame_id, x, y)?;
        // Hover effects, menus and tooltips can change the visual target even
        // when the pointer only moves. Invalidate before dispatch so a partial
        // OS-level failure can never make the old frame reusable.
        self.last_frame = None;
        platform::move_pointer(desktop_x, desktop_y)?;
        Ok((desktop_x, desktop_y))
    }

    pub fn click(
        &mut self,
        frame_id: &str,
        x: f64,
        y: f64,
        button: &str,
        count: u32,
    ) -> Result<(f64, f64), String> {
        self.verify_frame_unchanged(frame_id)?;
        let (desktop_x, desktop_y) = self.resolve_point(frame_id, x, y)?;
        self.last_frame = None;
        platform::click(desktop_x, desktop_y, button, count.clamp(1, 3))?;
        Ok((desktop_x, desktop_y))
    }

    pub fn drag(
        &mut self,
        frame_id: &str,
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
        duration_ms: u64,
    ) -> Result<(), String> {
        self.verify_frame_unchanged(frame_id)?;
        let (desktop_from_x, desktop_from_y) = self.resolve_point(frame_id, from_x, from_y)?;
        let (desktop_to_x, desktop_to_y) = self.resolve_point(frame_id, to_x, to_y)?;
        self.last_frame = None;
        platform::drag(
            desktop_from_x,
            desktop_from_y,
            desktop_to_x,
            desktop_to_y,
            duration_ms.clamp(100, 5_000),
        )?;
        Ok(())
    }

    pub fn scroll(&mut self, frame_id: &str, delta_x: i32, delta_y: i32) -> Result<(), String> {
        self.verify_frame_unchanged(frame_id)?;
        self.require_frame(frame_id)?;
        self.last_frame = None;
        platform::scroll(
            delta_x.clamp(-10_000, 10_000),
            delta_y.clamp(-10_000, 10_000),
        )?;
        Ok(())
    }

    pub fn type_text(&mut self, frame_id: &str, text: &str) -> Result<(), String> {
        self.verify_frame_unchanged(frame_id)?;
        self.require_frame(frame_id)?;
        if text.len() > 64 * 1024 {
            return Err("单次输入不能超过 64KB".into());
        }
        self.last_frame = None;
        platform::type_text(text)?;
        Ok(())
    }

    pub fn key(&mut self, frame_id: &str, key: &str, modifiers: &[String]) -> Result<(), String> {
        self.verify_frame_unchanged(frame_id)?;
        self.require_frame(frame_id)?;
        if key.is_empty() || key.chars().count() > 64 || modifiers.len() > 4 {
            return Err("按键参数无效".into());
        }
        self.last_frame = None;
        platform::key(key, modifiers)?;
        Ok(())
    }

    fn resolve_point(&self, frame_id: &str, x: f64, y: f64) -> Result<(f64, f64), String> {
        let frame = self.require_frame(frame_id)?;
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f64::from(frame.meta.image_width)
            || y >= f64::from(frame.meta.image_height)
        {
            return Err("操作坐标超出截图范围".into());
        }
        let desktop_x =
            frame.meta.origin_x + x * frame.meta.logical_width / f64::from(frame.meta.image_width);
        let desktop_y = frame.meta.origin_y
            + y * frame.meta.logical_height / f64::from(frame.meta.image_height);
        Ok((desktop_x, desktop_y))
    }

    pub fn invalidate_frame(&mut self) {
        self.last_frame = None;
    }

    pub fn frame_meta(&self, frame_id: &str) -> Result<FrameMeta, String> {
        Ok(self.require_frame(frame_id)?.meta.clone())
    }

    fn require_frame(&self, frame_id: &str) -> Result<&StoredFrame, String> {
        let frame = self
            .last_frame
            .as_ref()
            .filter(|frame| frame.meta.frame_id == frame_id)
            .ok_or_else(|| "截图已过期，请先重新调用 computer_screenshot".to_string())?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if now_ms.saturating_sub(frame.meta.captured_at_ms) > MAX_FRAME_AGE_MS {
            return Err("截图已超过 30 秒，为避免错位操作，请重新截图".into());
        }
        Ok(frame)
    }

    fn verify_frame_unchanged(&self, frame_id: &str) -> Result<(), String> {
        let stored = self.require_frame(frame_id)?;
        if let Some(target) = stored.foreground_target.as_ref() {
            platform::restore_foreground(target)?;
            // Activation is asynchronous on both macOS and Windows. Give the
            // compositor one short frame to settle before comparing pixels.
            std::thread::sleep(Duration::from_millis(180));
        }
        let captured = platform::capture(Some(&stored.meta.display_id))?;
        if (captured.origin_x - stored.meta.origin_x).abs() > f64::EPSILON
            || (captured.origin_y - stored.meta.origin_y).abs() > f64::EPSILON
            || (captured.logical_width - stored.meta.logical_width).abs() > f64::EPSILON
            || (captured.logical_height - stored.meta.logical_height).abs() > f64::EPSILON
        {
            return Err("显示器布局在截图后已变化，为避免错位操作请重新截图".into());
        }
        let image = RgbaImage::from_raw(captured.width, captured.height, captured.rgba)
            .ok_or_else(|| "屏幕验证像素数据不完整".to_string())?;
        let mut dynamic = DynamicImage::ImageRgba8(image);
        if dynamic.width() > MAX_SCREENSHOT_WIDTH || dynamic.height() > MAX_SCREENSHOT_HEIGHT {
            dynamic = dynamic.resize(
                MAX_SCREENSHOT_WIDTH,
                MAX_SCREENSHOT_HEIGHT,
                image::imageops::FilterType::Triangle,
            );
        }
        if dynamic.width() != stored.meta.image_width
            || dynamic.height() != stored.meta.image_height
            || !visual_signatures_match(&stored.visual_signature, &visual_signature(&dynamic))
        {
            return Err("屏幕内容在截图后已明显变化，为避免点错窗口或控件，请重新截图".into());
        }
        Ok(())
    }
}

fn visual_signature(image: &DynamicImage) -> Vec<u8> {
    image
        .resize_exact(32, 18, image::imageops::FilterType::Triangle)
        .to_luma8()
        .into_raw()
}

fn visual_signatures_match(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() || expected.is_empty() {
        return false;
    }
    let mut total_difference = 0_u64;
    let mut substantially_changed = 0_usize;
    for (&before, &after) in expected.iter().zip(actual) {
        let difference = before.abs_diff(after);
        total_difference += u64::from(difference);
        if difference > 48 {
            substantially_changed += 1;
        }
    }
    let mean_difference = total_difference as f64 / expected.len() as f64;
    let changed_ratio = substantially_changed as f64 / expected.len() as f64;
    mean_difference <= 12.0 && changed_ratio <= 0.20
}

#[derive(Debug)]
struct CapturedDisplay {
    display_id: String,
    origin_x: f64,
    origin_y: f64,
    logical_width: f64,
    logical_height: f64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use core_graphics::access::ScreenCaptureAccess;
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{
        CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, KeyCode,
        ScrollEventUnit,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use objc2_app_kit::NSRunningApplication;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    fn input_allowed() -> bool {
        // SAFETY: AXIsProcessTrusted takes no arguments and has no ownership
        // transfer. It is available on every supported macOS target.
        unsafe { AXIsProcessTrusted() }
    }

    pub fn capability() -> ComputerCapability {
        let screen_capture = ScreenCaptureAccess.preflight();
        let input_control = input_allowed();
        let desktop = super::super::accessibility::check_desktop_session();
        let reason =
            desktop
                .as_ref()
                .err()
                .cloned()
                .or_else(|| match (screen_capture, input_control) {
                    (true, true) => None,
                    (false, false) => Some(
                        "需要在“系统设置 > 隐私与安全性”中授予 EchoAgent 屏幕录制和辅助功能权限。"
                            .into(),
                    ),
                    (false, true) => Some("需要授予 EchoAgent 屏幕录制权限。".into()),
                    (true, false) => Some("需要授予 EchoAgent 辅助功能权限。".into()),
                });
        ComputerCapability {
            // Platform support and live user consent are separate. Keep the
            // mode selectable so the UI can guide first-time permission setup.
            available: true,
            desktop_available: Some(desktop.is_ok()),
            platform: "macOS".into(),
            accessibility: super::super::accessibility::capability(),
            screen_capture,
            input_control,
            reason,
        }
    }

    pub fn request_permissions() -> Result<(), String> {
        if !ScreenCaptureAccess.preflight() {
            let _ = ScreenCaptureAccess.request();
        }
        if !input_allowed() {
            open::that(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            )
            .map_err(|error| format!("打开 macOS 辅助功能设置失败：{error}"))?;
        }
        Ok(())
    }

    pub fn foreground_target() -> Option<ForegroundTarget> {
        let pid = super::super::accessibility::macos_foreground_pid()
            .ok()
            .flatten()?;
        let application = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        if pid <= 0 {
            return None;
        }
        let name = application
            .localizedName()
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("进程 {pid}"));
        Some(ForegroundTarget {
            id: pid.to_string(),
            name,
        })
    }

    pub fn restore_foreground(target: &ForegroundTarget) -> Result<(), String> {
        let pid = target
            .id
            .parse::<libc::pid_t>()
            .map_err(|_| "已记录的 macOS 目标进程无效".to_string())?;
        if super::super::accessibility::macos_foreground_pid()? == Some(pid) {
            return Ok(());
        }
        if super::super::accessibility::macos_restore_foreground(pid)? {
            Ok(())
        } else {
            Err(format!(
                "无法切回截图时的目标应用“{}”，操作已取消",
                target.name
            ))
        }
    }

    pub fn displays() -> Result<Vec<DisplayInfo>, String> {
        super::super::accessibility::check_desktop_session()?;
        let ids = CGDisplay::active_displays()
            .map_err(|code| format!("枚举显示器失败（CoreGraphics {code}）"))?;
        let main = CGDisplay::main();
        Ok(ids
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let display = CGDisplay::new(id);
                let bounds = display.bounds();
                let pixel_width = display.pixels_wide().min(u64::from(u32::MAX)) as u32;
                let pixel_height = display.pixels_high().min(u64::from(u32::MAX)) as u32;
                let logical_width = bounds.size.width;
                DisplayInfo {
                    id: id.to_string(),
                    name: format!("显示器 {}", index + 1),
                    primary: id == main.id,
                    origin_x: bounds.origin.x,
                    origin_y: bounds.origin.y,
                    logical_width,
                    logical_height: bounds.size.height,
                    pixel_width,
                    pixel_height,
                    scale_factor: if logical_width > 0.0 {
                        f64::from(pixel_width) / logical_width
                    } else {
                        1.0
                    },
                }
            })
            .collect())
    }

    pub fn capture(display_id: Option<&str>) -> Result<CapturedDisplay, String> {
        if !ScreenCaptureAccess.preflight() {
            return Err("尚未授予屏幕录制权限，请在“设置 > 安全与权限”中完成授权".into());
        }
        let infos = displays()?;
        let selected = match display_id {
            Some(id) => infos.into_iter().find(|display| display.id == id),
            None => infos
                .iter()
                .find(|display| display.primary)
                .cloned()
                .or_else(|| infos.into_iter().next()),
        }
        .ok_or_else(|| "指定的显示器不存在".to_string())?;
        let id = selected
            .id
            .parse::<u32>()
            .map_err(|_| "显示器 ID 无效".to_string())?;
        let image = CGDisplay::new(id)
            .image()
            .ok_or_else(|| "截取显示器画面失败，请检查屏幕录制权限".to_string())?;
        let width = u32::try_from(image.width()).map_err(|_| "截图宽度超出上限")?;
        let height = u32::try_from(image.height()).map_err(|_| "截图高度超出上限")?;
        if image.bits_per_pixel() != 32 {
            return Err(format!(
                "不支持的屏幕像素格式：{} bpp",
                image.bits_per_pixel()
            ));
        }
        let bytes_per_row = image.bytes_per_row();
        let data = image.data();
        let raw = data.bytes();
        let required = bytes_per_row
            .checked_mul(height as usize)
            .ok_or_else(|| "截图尺寸溢出".to_string())?;
        if raw.len() < required {
            return Err("屏幕截图像素数据不完整".into());
        }
        let mut rgba = vec![0_u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            let src = &raw[y * bytes_per_row..y * bytes_per_row + width as usize * 4];
            let dst = &mut rgba[y * width as usize * 4..(y + 1) * width as usize * 4];
            for x in 0..width as usize {
                let source = &src[x * 4..x * 4 + 4];
                let target = &mut dst[x * 4..x * 4 + 4];
                // CGDisplayCreateImage returns native-endian premultiplied BGRA
                // on supported macOS displays.
                target[0] = source[2];
                target[1] = source[1];
                target[2] = source[0];
                target[3] = source[3];
            }
        }
        Ok(CapturedDisplay {
            display_id: selected.id,
            origin_x: selected.origin_x,
            origin_y: selected.origin_y,
            logical_width: selected.logical_width,
            logical_height: selected.logical_height,
            width,
            height,
            rgba,
        })
    }

    fn source() -> Result<CGEventSource, String> {
        if !input_allowed() {
            return Err("尚未授予辅助功能权限，请在系统设置中授予本应用辅助功能权限".into());
        }
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "创建 macOS 输入事件源失败".to_string())
    }

    fn point(x: f64, y: f64) -> core_graphics::geometry::CGPoint {
        core_graphics::geometry::CGPoint::new(x, y)
    }

    pub fn move_pointer(x: f64, y: f64) -> Result<(), String> {
        let event = CGEvent::new_mouse_event(
            source()?,
            CGEventType::MouseMoved,
            point(x, y),
            CGMouseButton::Left,
        )
        .map_err(|_| "创建鼠标移动事件失败".to_string())?;
        event.set_flags(CGEventFlags::CGEventFlagNull);
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn click_event(
        source: CGEventSource,
        kind: CGEventType,
        position: core_graphics::geometry::CGPoint,
        button: CGMouseButton,
        click_number: i64,
    ) -> Result<CGEvent, String> {
        let event = CGEvent::new_mouse_event(source, kind, position, button)
            .map_err(|_| "创建鼠标点击事件失败".to_string())?;
        event.set_flags(CGEventFlags::CGEventFlagNull);
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_number);
        Ok(event)
    }

    pub fn click(x: f64, y: f64, button: &str, count: u32) -> Result<(), String> {
        let (mouse_button, down, up) = match button {
            "left" => (
                CGMouseButton::Left,
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
            ),
            "right" => (
                CGMouseButton::Right,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
            ),
            "middle" => (
                CGMouseButton::Center,
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
            ),
            _ => return Err("鼠标按键只能是 left、right 或 middle".into()),
        };
        move_pointer(x, y)?;
        for click_number in 1..=count {
            // Construct both halves first so allocation failure cannot strand
            // a pressed button. macOS requires 1,2,... click-state metadata;
            // repeated single-click events are not a native double click.
            let down_event = click_event(
                source()?,
                down,
                point(x, y),
                mouse_button,
                i64::from(click_number),
            )?;
            let up_event = click_event(
                source()?,
                up,
                point(x, y),
                mouse_button,
                i64::from(click_number),
            )?;
            down_event.post(CGEventTapLocation::HID);
            up_event.post(CGEventTapLocation::HID);
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    }

    #[cfg(test)]
    mod click_tests {
        use super::*;
        #[test]
        fn macos_double_click_events_have_native_click_counts_and_neutral_flags() {
            for number in [1, 2] {
                for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
                    let event = click_event(
                        CGEventSource::new(CGEventSourceStateID::Private).unwrap(),
                        kind,
                        point(10.0, 20.0),
                        CGMouseButton::Left,
                        number,
                    )
                    .unwrap();
                    assert_eq!(
                        event.get_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE),
                        number
                    );
                    assert_eq!(event.get_flags(), CGEventFlags::CGEventFlagNull);
                }
            }
        }
    }

    pub fn drag(
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
        duration_ms: u64,
    ) -> Result<(), String> {
        move_pointer(from_x, from_y)?;
        CGEvent::new_mouse_event(
            source()?,
            CGEventType::LeftMouseDown,
            point(from_x, from_y),
            CGMouseButton::Left,
        )
        .map_err(|_| "创建拖拽按下事件失败".to_string())?
        .post(CGEventTapLocation::HID);
        let steps = (duration_ms / 16).clamp(4, 120);
        let movement = (|| {
            for step in 1..=steps {
                let ratio = step as f64 / steps as f64;
                let x = from_x + (to_x - from_x) * ratio;
                let y = from_y + (to_y - from_y) * ratio;
                CGEvent::new_mouse_event(
                    source()?,
                    CGEventType::LeftMouseDragged,
                    point(x, y),
                    CGMouseButton::Left,
                )
                .map_err(|_| "创建拖拽移动事件失败".to_string())?
                .post(CGEventTapLocation::HID);
                std::thread::sleep(Duration::from_millis(16));
            }
            Ok(())
        })();
        let release = CGEvent::new_mouse_event(
            source()?,
            CGEventType::LeftMouseUp,
            point(to_x, to_y),
            CGMouseButton::Left,
        )
        .map_err(|_| "创建拖拽释放事件失败".to_string())
        .map(|event| event.post(CGEventTapLocation::HID));
        movement.and(release)
    }

    pub fn scroll(delta_x: i32, delta_y: i32) -> Result<(), String> {
        CGEvent::new_scroll_event(source()?, ScrollEventUnit::PIXEL, 2, -delta_y, -delta_x, 0)
            .map_err(|_| "创建滚动事件失败".to_string())?
            .post(CGEventTapLocation::HID);
        Ok(())
    }

    pub fn type_text(text: &str) -> Result<(), String> {
        let down = CGEvent::new_keyboard_event(source()?, 0, true)
            .map_err(|_| "创建键盘输入事件失败".to_string())?;
        down.set_flags(CGEventFlags::CGEventFlagNull);
        down.set_string(text);
        down.post(CGEventTapLocation::HID);
        let up = CGEvent::new_keyboard_event(source()?, 0, false)
            .map_err(|_| "创建键盘释放事件失败".to_string())?;
        up.set_flags(CGEventFlags::CGEventFlagNull);
        up.set_string(text);
        up.post(CGEventTapLocation::HID);
        Ok(())
    }

    pub fn key(key: &str, modifiers: &[String]) -> Result<(), String> {
        let keycode = keycode(key)?;
        let flags = modifier_flags(modifiers)?;
        let down = CGEvent::new_keyboard_event(source()?, keycode, true)
            .map_err(|_| "创建按键事件失败".to_string())?;
        down.set_flags(flags);
        down.post(CGEventTapLocation::HID);
        let up = CGEvent::new_keyboard_event(source()?, keycode, false)
            .map_err(|_| "创建按键释放事件失败".to_string())?;
        up.set_flags(flags);
        up.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn modifier_flags(modifiers: &[String]) -> Result<CGEventFlags, String> {
        let mut flags = CGEventFlags::CGEventFlagNull;
        for modifier in modifiers {
            flags |= match modifier.to_ascii_lowercase().as_str() {
                "shift" => CGEventFlags::CGEventFlagShift,
                "control" | "ctrl" => CGEventFlags::CGEventFlagControl,
                "alt" | "option" => CGEventFlags::CGEventFlagAlternate,
                "meta" | "command" | "cmd" => CGEventFlags::CGEventFlagCommand,
                other => return Err(format!("不支持的修饰键：{other}")),
            };
        }
        Ok(flags)
    }

    fn keycode(key: &str) -> Result<u16, String> {
        let code = match key.to_ascii_lowercase().as_str() {
            "a" => KeyCode::ANSI_A,
            "b" => KeyCode::ANSI_B,
            "c" => KeyCode::ANSI_C,
            "d" => KeyCode::ANSI_D,
            "e" => KeyCode::ANSI_E,
            "f" => KeyCode::ANSI_F,
            "g" => KeyCode::ANSI_G,
            "h" => KeyCode::ANSI_H,
            "i" => KeyCode::ANSI_I,
            "j" => KeyCode::ANSI_J,
            "k" => KeyCode::ANSI_K,
            "l" => KeyCode::ANSI_L,
            "m" => KeyCode::ANSI_M,
            "n" => KeyCode::ANSI_N,
            "o" => KeyCode::ANSI_O,
            "p" => KeyCode::ANSI_P,
            "q" => KeyCode::ANSI_Q,
            "r" => KeyCode::ANSI_R,
            "s" => KeyCode::ANSI_S,
            "t" => KeyCode::ANSI_T,
            "u" => KeyCode::ANSI_U,
            "v" => KeyCode::ANSI_V,
            "w" => KeyCode::ANSI_W,
            "x" => KeyCode::ANSI_X,
            "y" => KeyCode::ANSI_Y,
            "z" => KeyCode::ANSI_Z,
            "0" => KeyCode::ANSI_0,
            "1" => KeyCode::ANSI_1,
            "2" => KeyCode::ANSI_2,
            "3" => KeyCode::ANSI_3,
            "4" => KeyCode::ANSI_4,
            "5" => KeyCode::ANSI_5,
            "6" => KeyCode::ANSI_6,
            "7" => KeyCode::ANSI_7,
            "8" => KeyCode::ANSI_8,
            "9" => KeyCode::ANSI_9,
            "enter" | "return" => KeyCode::RETURN,
            "tab" => KeyCode::TAB,
            "space" => KeyCode::SPACE,
            "backspace" | "deletebackward" => KeyCode::DELETE,
            "delete" | "deleteforward" => KeyCode::FORWARD_DELETE,
            "escape" | "esc" => KeyCode::ESCAPE,
            "left" | "arrowleft" => KeyCode::LEFT_ARROW,
            "right" | "arrowright" => KeyCode::RIGHT_ARROW,
            "up" | "arrowup" => KeyCode::UP_ARROW,
            "down" | "arrowdown" => KeyCode::DOWN_ARROW,
            "home" => KeyCode::HOME,
            "end" => KeyCode::END,
            "pageup" => KeyCode::PAGE_UP,
            "pagedown" => KeyCode::PAGE_DOWN,
            other => return Err(format!("不支持的按键：{other}")),
        };
        Ok(code)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub fn capability() -> ComputerCapability {
        use windows_sys::Win32::System::StationsAndDesktops::{
            CloseDesktop, OpenInputDesktop, DESKTOP_READOBJECTS,
        };
        let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
        let interactive = !desktop.is_null();
        if interactive {
            unsafe {
                CloseDesktop(desktop);
            }
        }
        let screen_capture = interactive && displays().is_ok();
        ComputerCapability {
            available: true,
            desktop_available: Some(interactive),
            platform: "Windows".into(),
            accessibility: super::super::accessibility::capability(),
            screen_capture,
            input_control: interactive,
            reason: if interactive {
                Some("已探测交互桌面；目标应用的权限级别及 UIA 可用性仍需逐次验证。".into())
            } else {
                Some("无法打开 Windows 交互桌面；检查锁屏、UAC 安全桌面及会话权限。".into())
            },
        }
    }

    pub fn request_permissions() -> Result<(), String> {
        Ok(())
    }

    pub fn foreground_target() -> Option<ForegroundTarget> {
        let window = unsafe { GetForegroundWindow() };
        if window.is_null() {
            return None;
        }
        let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
        let mut buffer = vec![0_u16; length.saturating_add(1)];
        let copied = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        let name = if copied > 0 {
            String::from_utf16_lossy(&buffer[..copied as usize])
        } else {
            "Windows 应用".to_string()
        };
        Some(ForegroundTarget {
            id: (window as usize).to_string(),
            name,
        })
    }

    pub fn restore_foreground(target: &ForegroundTarget) -> Result<(), String> {
        let address = target
            .id
            .parse::<usize>()
            .map_err(|_| "已记录的 Windows 目标窗口无效".to_string())?;
        let window = address as windows_sys::Win32::Foundation::HWND;
        if unsafe { IsWindow(window) } == 0 {
            return Err(format!("目标窗口“{}”已关闭，请重新截图", target.name));
        }
        if unsafe { GetForegroundWindow() } == window {
            return Ok(());
        }
        unsafe {
            ShowWindow(window, SW_RESTORE);
        }
        if unsafe { SetForegroundWindow(window) } == 0 {
            Err(format!(
                "无法切回截图时的目标窗口“{}”，操作已取消",
                target.name
            ))
        } else {
            Ok(())
        }
    }

    pub fn displays() -> Result<Vec<DisplayInfo>, String> {
        unsafe extern "system" fn collect_monitor(
            monitor: HMONITOR,
            _dc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            let monitors = unsafe { &mut *(data as *mut Vec<(HMONITOR, MONITORINFO)>) };
            let mut info: MONITORINFO = unsafe { zeroed() };
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
                monitors.push((monitor, info));
            }
            1
        }

        let mut monitors = Vec::<(HMONITOR, MONITORINFO)>::new();
        let enumerated = unsafe {
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(collect_monitor),
                (&mut monitors as *mut Vec<(HMONITOR, MONITORINFO)>) as LPARAM,
            )
        };
        if enumerated == 0 || monitors.is_empty() {
            return Err("无法枚举 Windows 显示器".into());
        }
        Ok(monitors
            .into_iter()
            .enumerate()
            .filter_map(|(index, (monitor, info))| {
                let width = info.rcMonitor.right - info.rcMonitor.left;
                let height = info.rcMonitor.bottom - info.rcMonitor.top;
                (width > 0 && height > 0).then(|| DisplayInfo {
                    id: (monitor as usize).to_string(),
                    name: format!("Windows 显示器 {}", index + 1),
                    primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                    origin_x: f64::from(info.rcMonitor.left),
                    origin_y: f64::from(info.rcMonitor.top),
                    logical_width: f64::from(width),
                    logical_height: f64::from(height),
                    pixel_width: width as u32,
                    pixel_height: height as u32,
                    // PerMonitorV2 awareness makes these physical desktop
                    // coordinates. Keeping logical and captured pixels equal
                    // avoids the mixed-DPI offset bug of the old virtual canvas.
                    scale_factor: 1.0,
                })
            })
            .collect())
    }

    pub fn capture(display_id: Option<&str>) -> Result<CapturedDisplay, String> {
        let displays = displays()?;
        let selected = match display_id {
            Some(id) => displays.into_iter().find(|display| display.id == id),
            None => displays
                .iter()
                .find(|display| display.primary)
                .cloned()
                .or_else(|| displays.into_iter().next()),
        }
        .ok_or_else(|| "指定的显示器不存在".to_string())?;
        let x = selected.origin_x.round() as i32;
        let y = selected.origin_y.round() as i32;
        let width = selected.pixel_width as i32;
        let height = selected.pixel_height as i32;
        unsafe {
            let screen_dc = GetDC(null_mut());
            if screen_dc.is_null() {
                return Err("获取 Windows 屏幕 DC 失败".into());
            }
            let memory_dc = CreateCompatibleDC(screen_dc);
            let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
            if memory_dc.is_null() || bitmap.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                if !memory_dc.is_null() {
                    DeleteDC(memory_dc);
                }
                ReleaseDC(null_mut(), screen_dc);
                return Err("创建 Windows 截图缓冲区失败".into());
            }
            let old = SelectObject(memory_dc, bitmap);
            let copied = BitBlt(
                memory_dc,
                0,
                0,
                width,
                height,
                screen_dc,
                x,
                y,
                SRCCOPY | CAPTUREBLT,
            );
            let mut info: BITMAPINFO = zeroed();
            info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bgra = vec![0_u8; width as usize * height as usize * 4];
            let lines = if copied != 0 {
                GetDIBits(
                    memory_dc,
                    bitmap,
                    0,
                    height as u32,
                    bgra.as_mut_ptr().cast(),
                    &mut info,
                    DIB_RGB_COLORS,
                )
            } else {
                0
            };
            SelectObject(memory_dc, old);
            DeleteObject(bitmap);
            DeleteDC(memory_dc);
            ReleaseDC(null_mut(), screen_dc);
            if lines != height {
                return Err("读取 Windows 屏幕像素失败".into());
            }
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
                pixel[3] = 255;
            }
            Ok(CapturedDisplay {
                display_id: selected.id,
                origin_x: selected.origin_x,
                origin_y: selected.origin_y,
                logical_width: selected.logical_width,
                logical_height: selected.logical_height,
                width: width as u32,
                height: height as u32,
                rgba: bgra,
            })
        }
    }

    pub fn move_pointer(x: f64, y: f64) -> Result<(), String> {
        let ok = unsafe { SetCursorPos(x.round() as i32, y.round() as i32) };
        if ok == 0 {
            Err("移动 Windows 鼠标失败".into())
        } else {
            Ok(())
        }
    }

    fn mouse(flags: MOUSE_EVENT_FLAGS, data: u32) -> Result<(), String> {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent = unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err("发送 Windows 鼠标事件失败".into())
        }
    }

    pub fn click(x: f64, y: f64, button: &str, count: u32) -> Result<(), String> {
        move_pointer(x, y)?;
        let (down, up) = match button {
            "left" => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            "right" => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            "middle" => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
            _ => return Err("鼠标按键只能是 left、right 或 middle".into()),
        };
        for _ in 0..count {
            mouse(down, 0)?;
            mouse(up, 0)?;
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    }

    pub fn drag(
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
        duration_ms: u64,
    ) -> Result<(), String> {
        move_pointer(from_x, from_y)?;
        mouse(MOUSEEVENTF_LEFTDOWN, 0)?;
        let steps = (duration_ms / 16).clamp(4, 120);
        let movement = (|| {
            for step in 1..=steps {
                let ratio = step as f64 / steps as f64;
                move_pointer(
                    from_x + (to_x - from_x) * ratio,
                    from_y + (to_y - from_y) * ratio,
                )?;
                std::thread::sleep(Duration::from_millis(16));
            }
            Ok(())
        })();
        let release = mouse(MOUSEEVENTF_LEFTUP, 0);
        movement.and(release)
    }

    pub fn scroll(delta_x: i32, delta_y: i32) -> Result<(), String> {
        if delta_y != 0 {
            mouse(MOUSEEVENTF_WHEEL, (-delta_y) as u32)?;
        }
        if delta_x != 0 {
            mouse(MOUSEEVENTF_HWHEEL, delta_x as u32)?;
        }
        Ok(())
    }

    fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> Result<(), String> {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        if unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) } == 1 {
            Ok(())
        } else {
            Err("发送 Windows 键盘事件失败".into())
        }
    }

    pub fn type_text(text: &str) -> Result<(), String> {
        for unit in text.encode_utf16() {
            keyboard(0, unit, KEYEVENTF_UNICODE)?;
            keyboard(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)?;
        }
        Ok(())
    }

    pub fn key(key: &str, modifiers: &[String]) -> Result<(), String> {
        let vk = virtual_key(key)?;
        let mods = modifiers
            .iter()
            .map(|modifier| match modifier.to_ascii_lowercase().as_str() {
                "shift" => Ok(VK_SHIFT),
                "control" | "ctrl" => Ok(VK_CONTROL),
                "alt" | "option" => Ok(VK_MENU),
                "meta" | "command" | "cmd" => Ok(VK_LWIN),
                other => Err(format!("不支持的修饰键：{other}")),
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut pressed = Vec::new();
        for &modifier in &mods {
            if let Err(error) = keyboard(modifier, 0, 0) {
                for &held in pressed.iter().rev() {
                    let _ = keyboard(held, 0, KEYEVENTF_KEYUP);
                }
                return Err(error);
            }
            pressed.push(modifier);
        }
        let key_result = keyboard(vk, 0, 0).and_then(|_| keyboard(vk, 0, KEYEVENTF_KEYUP));
        let mut release_error = None;
        for &modifier in pressed.iter().rev() {
            if let Err(error) = keyboard(modifier, 0, KEYEVENTF_KEYUP) {
                release_error.get_or_insert(error);
            }
        }
        key_result.and_then(|_| release_error.map_or(Ok(()), Err))
    }

    fn virtual_key(key: &str) -> Result<u16, String> {
        Ok(match key.to_ascii_lowercase().as_str() {
            "enter" | "return" => VK_RETURN,
            "tab" => VK_TAB,
            "space" => VK_SPACE,
            "backspace" => VK_BACK,
            "delete" => VK_DELETE,
            "escape" | "esc" => VK_ESCAPE,
            "left" | "arrowleft" => VK_LEFT,
            "right" | "arrowright" => VK_RIGHT,
            "up" | "arrowup" => VK_UP,
            "down" | "arrowdown" => VK_DOWN,
            "home" => VK_HOME,
            "end" => VK_END,
            "pageup" => VK_PRIOR,
            "pagedown" => VK_NEXT,
            "insert" => VK_INSERT,
            "printscreen" => VK_SNAPSHOT,
            ";" | ":" => VK_OEM_1,
            "=" | "+" => VK_OEM_PLUS,
            "," | "<" => VK_OEM_COMMA,
            "-" | "_" => VK_OEM_MINUS,
            "." | ">" => VK_OEM_PERIOD,
            "/" | "?" => VK_OEM_2,
            "`" | "~" => VK_OEM_3,
            "[" | "{" => VK_OEM_4,
            "\\" | "|" => VK_OEM_5,
            "]" | "}" => VK_OEM_6,
            "'" | "\"" => VK_OEM_7,
            value
                if value.len() == 1
                    && value
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric) =>
            {
                value.as_bytes()[0].to_ascii_uppercase() as u16
            }
            value if value.starts_with('f') && value[1..].parse::<u16>().is_ok() => {
                let number = value[1..].parse::<u16>().unwrap_or_default();
                if !(1..=24).contains(&number) {
                    return Err(format!("不支持的按键：{key}"));
                }
                VK_F1 + number - 1
            }
            other => return Err(format!("不支持的按键：{other}")),
        })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ConfigureWindowAux, ConnectionExt as _, ImageFormat, ImageOrder, InputFocus,
        StackMode, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
        MOTION_NOTIFY_EVENT,
    };
    use x11rb::protocol::xtest::ConnectionExt as _;
    use x11rb::rust_connection::RustConnection;
    use x11rb::{connect, CURRENT_TIME};

    fn wayland_session() -> bool {
        std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value.eq_ignore_ascii_case("wayland"))
            || (std::env::var_os("WAYLAND_DISPLAY").is_some()
                && std::env::var_os("DISPLAY").is_none())
    }

    fn connect_x11() -> Result<(RustConnection, usize), String> {
        if wayland_session() {
            return Err(
                "当前是 Wayland 会话。为避免绕过桌面安全边界，操作电脑功能只在 Linux X11 会话中启用；请登录“Xorg/X11”会话后重试。".into(),
            );
        }
        connect(None).map_err(|error| format!("连接 Linux X11 桌面失败：{error}"))
    }

    fn connection_with_xtest() -> Result<(RustConnection, usize), String> {
        let (connection, screen) = connect_x11()?;
        connection
            .xtest_get_version(2, 2)
            .map_err(|error| format!("检查 XTEST 扩展失败：{error}"))?
            .reply()
            .map_err(|error| format!("X11 服务器未提供 XTEST 输入扩展：{error}"))?;
        Ok((connection, screen))
    }

    pub fn capability() -> ComputerCapability {
        let result = connection_with_xtest();
        let reason = result.as_ref().err().cloned();
        ComputerCapability {
            available: result.is_ok(),
            desktop_available: Some(result.is_ok()),
            platform: "Linux X11".into(),
            accessibility: super::super::accessibility::capability(),
            screen_capture: result.is_ok(),
            input_control: result.is_ok(),
            reason,
        }
    }

    pub fn request_permissions() -> Result<(), String> {
        connection_with_xtest().map(|_| ())
    }

    pub fn foreground_target() -> Option<ForegroundTarget> {
        let (connection, screen_index) = connect_x11().ok()?;
        let screen = connection.setup().roots.get(screen_index)?;
        let active_atom = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let window = connection
            .get_property(false, screen.root, active_atom, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?
            .value32()?
            .next()?;
        let name_atom = connection
            .intern_atom(false, b"_NET_WM_NAME")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let name = connection
            .get_property(false, window, name_atom, AtomEnum::ANY, 0, 512)
            .ok()?
            .reply()
            .ok()
            .and_then(|reply| String::from_utf8(reply.value).ok())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("X11 窗口 {window}"));
        Some(ForegroundTarget {
            id: window.to_string(),
            name,
        })
    }

    pub fn restore_foreground(target: &ForegroundTarget) -> Result<(), String> {
        let (connection, _) = connection_with_xtest()?;
        let window = target
            .id
            .parse::<u32>()
            .map_err(|_| "已记录的 X11 目标窗口无效".to_string())?;
        connection
            .configure_window(
                window,
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            )
            .map_err(|error| format!("置顶 X11 目标窗口失败：{error}"))?
            .check()
            .map_err(|error| format!("目标窗口“{}”已不可用：{error}", target.name))?;
        connection
            .set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
            .map_err(|error| format!("聚焦 X11 目标窗口失败：{error}"))?
            .check()
            .map_err(|error| format!("无法切回目标窗口“{}”：{error}", target.name))?;
        connection
            .flush()
            .map_err(|error| format!("刷新 X11 窗口状态失败：{error}"))
    }

    pub fn displays() -> Result<Vec<DisplayInfo>, String> {
        let (connection, screen_index) = connect_x11()?;
        let screen = connection
            .setup()
            .roots
            .get(screen_index)
            .ok_or_else(|| "X11 默认屏幕不存在".to_string())?;
        Ok(vec![DisplayInfo {
            id: screen_index.to_string(),
            name: format!("X11 桌面 {}", screen_index + 1),
            primary: true,
            origin_x: 0.0,
            origin_y: 0.0,
            logical_width: f64::from(screen.width_in_pixels),
            logical_height: f64::from(screen.height_in_pixels),
            pixel_width: u32::from(screen.width_in_pixels),
            pixel_height: u32::from(screen.height_in_pixels),
            scale_factor: 1.0,
        }])
    }

    pub fn capture(display_id: Option<&str>) -> Result<CapturedDisplay, String> {
        let (connection, screen_index) = connect_x11()?;
        if display_id.is_some_and(|id| id != screen_index.to_string()) {
            return Err("指定的 X11 显示器不存在".into());
        }
        let setup = connection.setup();
        let screen = setup
            .roots
            .get(screen_index)
            .ok_or_else(|| "X11 默认屏幕不存在".to_string())?;
        let width = screen.width_in_pixels;
        let height = screen.height_in_pixels;
        let reply = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                screen.root,
                0,
                0,
                width,
                height,
                u32::MAX,
            )
            .map_err(|error| format!("请求 X11 屏幕像素失败：{error}"))?
            .reply()
            .map_err(|error| format!("读取 X11 屏幕像素失败：{error}"))?;
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == reply.depth)
            .ok_or_else(|| format!("不支持的 X11 像素深度：{}", reply.depth))?;
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|depth| depth.visuals.iter())
            .find(|visual| {
                visual.visual_id == reply.visual || visual.visual_id == screen.root_visual
            })
            .ok_or_else(|| "无法读取 X11 TrueColor 格式".to_string())?;
        let bytes_per_pixel = usize::from(format.bits_per_pixel.div_ceil(8));
        if !matches!(bytes_per_pixel, 2..=4) {
            return Err(format!(
                "不支持的 X11 每像素位数：{}",
                format.bits_per_pixel
            ));
        }
        let pad = usize::from(format.scanline_pad);
        if pad == 0 {
            return Err("X11 像素行对齐参数无效".into());
        }
        let row_bits = usize::from(width) * usize::from(format.bits_per_pixel);
        let stride = row_bits.div_ceil(pad) * pad / 8;
        if reply.data.len() < stride * usize::from(height) {
            return Err("X11 截图像素数据不完整".into());
        }
        let mut rgba = vec![0_u8; usize::from(width) * usize::from(height) * 4];
        for y in 0..usize::from(height) {
            for x in 0..usize::from(width) {
                let offset = y * stride + x * bytes_per_pixel;
                let bytes = &reply.data[offset..offset + bytes_per_pixel];
                let pixel = if u8::from(setup.image_byte_order) == u8::from(ImageOrder::LSB_FIRST) {
                    bytes
                        .iter()
                        .enumerate()
                        .fold(0_u32, |value, (index, byte)| {
                            value | (u32::from(*byte) << (index * 8))
                        })
                } else {
                    bytes
                        .iter()
                        .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte))
                };
                let destination = (y * usize::from(width) + x) * 4;
                rgba[destination] = channel(pixel, visual.red_mask);
                rgba[destination + 1] = channel(pixel, visual.green_mask);
                rgba[destination + 2] = channel(pixel, visual.blue_mask);
                rgba[destination + 3] = 255;
            }
        }
        Ok(CapturedDisplay {
            display_id: screen_index.to_string(),
            origin_x: 0.0,
            origin_y: 0.0,
            logical_width: f64::from(width),
            logical_height: f64::from(height),
            width: u32::from(width),
            height: u32::from(height),
            rgba,
        })
    }

    fn channel(pixel: u32, mask: u32) -> u8 {
        if mask == 0 {
            return 0;
        }
        let shifted = (pixel & mask) >> mask.trailing_zeros();
        let maximum = mask >> mask.trailing_zeros();
        ((u64::from(shifted) * 255) / u64::from(maximum)) as u8
    }

    fn fake_input(type_: u8, detail: u8, x: i16, y: i16) -> Result<(), String> {
        let (connection, screen_index) = connection_with_xtest()?;
        let root = connection.setup().roots[screen_index].root;
        connection
            .xtest_fake_input(type_, detail, CURRENT_TIME, root, x, y, 0)
            .map_err(|error| format!("发送 X11 输入事件失败：{error}"))?
            .check()
            .map_err(|error| format!("X11 输入事件被拒绝：{error}"))?;
        connection
            .flush()
            .map_err(|error| format!("刷新 X11 输入失败：{error}"))
    }

    pub fn move_pointer(x: f64, y: f64) -> Result<(), String> {
        fake_input(
            MOTION_NOTIFY_EVENT,
            0,
            x.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
            y.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
        )
    }

    fn mouse_button(button: u8, pressed: bool) -> Result<(), String> {
        fake_input(
            if pressed {
                BUTTON_PRESS_EVENT
            } else {
                BUTTON_RELEASE_EVENT
            },
            button,
            0,
            0,
        )
    }

    pub fn click(x: f64, y: f64, button: &str, count: u32) -> Result<(), String> {
        let button = match button {
            "left" => 1,
            "middle" => 2,
            "right" => 3,
            _ => return Err("鼠标按键只能是 left、right 或 middle".into()),
        };
        move_pointer(x, y)?;
        for _ in 0..count {
            mouse_button(button, true)?;
            mouse_button(button, false)?;
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    }

    pub fn drag(
        from_x: f64,
        from_y: f64,
        to_x: f64,
        to_y: f64,
        duration_ms: u64,
    ) -> Result<(), String> {
        move_pointer(from_x, from_y)?;
        mouse_button(1, true)?;
        let steps = (duration_ms / 16).clamp(4, 120);
        let movement = (|| {
            for step in 1..=steps {
                let ratio = step as f64 / steps as f64;
                move_pointer(
                    from_x + (to_x - from_x) * ratio,
                    from_y + (to_y - from_y) * ratio,
                )?;
                std::thread::sleep(Duration::from_millis(16));
            }
            Ok(())
        })();
        let release = mouse_button(1, false);
        movement.and(release)
    }

    pub fn scroll(delta_x: i32, delta_y: i32) -> Result<(), String> {
        for (delta, negative, positive) in [(delta_y, 4_u8, 5_u8), (delta_x, 6_u8, 7_u8)] {
            let button = if delta < 0 { negative } else { positive };
            let steps = (delta.unsigned_abs().div_ceil(100)).clamp(1, 100);
            if delta != 0 {
                for _ in 0..steps {
                    mouse_button(button, true)?;
                    mouse_button(button, false)?;
                }
            }
        }
        Ok(())
    }

    pub fn type_text(text: &str) -> Result<(), String> {
        let (connection, screen_index) = connection_with_xtest()?;
        let setup = connection.setup();
        let root = setup.roots[screen_index].root;
        let count = setup
            .max_keycode
            .saturating_sub(setup.min_keycode)
            .saturating_add(1);
        let mapping = connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|error| format!("读取 X11 键盘映射失败：{error}"))?
            .reply()
            .map_err(|error| format!("读取 X11 键盘映射失败：{error}"))?;
        let per_key = usize::from(mapping.keysyms_per_keycode);
        if per_key == 0 {
            return Err("X11 键盘映射为空".into());
        }
        let slot_index = mapping
            .keysyms
            .chunks(per_key)
            .rposition(|symbols| symbols.iter().all(|symbol| *symbol == 0))
            .unwrap_or_else(|| usize::from(count.saturating_sub(1)));
        let slot_offset = slot_index * per_key;
        let original = mapping
            .keysyms
            .get(slot_offset..slot_offset + per_key)
            .ok_or_else(|| "X11 键盘映射数据不完整".to_string())?
            .to_vec();
        let keycode = setup
            .min_keycode
            .checked_add(
                u8::try_from(slot_index).map_err(|_| "X11 键盘映射索引超出范围".to_string())?,
            )
            .ok_or_else(|| "X11 键码超出范围".to_string())?;

        let send = |type_, code| -> Result<(), String> {
            connection
                .xtest_fake_input(type_, code, CURRENT_TIME, root, 0, 0, 0)
                .map_err(|error| format!("发送 X11 键盘事件失败：{error}"))?
                .check()
                .map_err(|error| format!("X11 键盘事件被拒绝：{error}"))
        };
        let type_result = (|| {
            for character in text.chars() {
                let codepoint = character as u32;
                let keysym = if codepoint <= 0xff {
                    codepoint
                } else {
                    0x0100_0000 | codepoint
                };
                let mut temporary = vec![0_u32; per_key];
                temporary[0] = keysym;
                connection
                    .change_keyboard_mapping(1, keycode, mapping.keysyms_per_keycode, &temporary)
                    .map_err(|error| format!("设置 X11 Unicode 键位失败：{error}"))?
                    .check()
                    .map_err(|error| format!("设置 X11 Unicode 键位失败：{error}"))?;
                send(KEY_PRESS_EVENT, keycode)?;
                send(KEY_RELEASE_EVENT, keycode)?;
            }
            connection
                .flush()
                .map_err(|error| format!("刷新 X11 文本输入失败：{error}"))
        })();
        let restore_result = connection
            .change_keyboard_mapping(1, keycode, mapping.keysyms_per_keycode, &original)
            .map_err(|error| format!("恢复 X11 键盘映射失败：{error}"))
            .and_then(|cookie| {
                cookie
                    .check()
                    .map_err(|error| format!("恢复 X11 键盘映射失败：{error}"))
            })
            .and_then(|_| {
                connection
                    .flush()
                    .map_err(|error| format!("刷新 X11 键盘映射失败：{error}"))
            });
        type_result.and(restore_result)
    }

    pub fn key(key: &str, modifiers: &[String]) -> Result<(), String> {
        let (connection, screen_index) = connection_with_xtest()?;
        let root = connection.setup().roots[screen_index].root;
        let keycode = find_keycode(&connection, key_keysym(key)?)?;
        let modifier_codes = modifiers
            .iter()
            .map(|modifier| {
                modifier_keysym(modifier).and_then(|sym| find_keycode(&connection, sym))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let send = |type_, code| -> Result<(), String> {
            connection
                .xtest_fake_input(type_, code, CURRENT_TIME, root, 0, 0, 0)
                .map_err(|error| format!("发送 X11 键盘事件失败：{error}"))?
                .check()
                .map_err(|error| format!("X11 键盘事件被拒绝：{error}"))
        };
        let mut pressed = Vec::new();
        for code in modifier_codes {
            if let Err(error) = send(KEY_PRESS_EVENT, code) {
                for held in pressed.into_iter().rev() {
                    let _ = send(KEY_RELEASE_EVENT, held);
                }
                return Err(error);
            }
            pressed.push(code);
        }
        let result = send(KEY_PRESS_EVENT, keycode).and_then(|_| send(KEY_RELEASE_EVENT, keycode));
        for held in pressed.into_iter().rev() {
            let _ = send(KEY_RELEASE_EVENT, held);
        }
        connection
            .flush()
            .map_err(|error| format!("刷新 X11 键盘输入失败：{error}"))?;
        result
    }

    fn find_keycode(connection: &RustConnection, keysym: u32) -> Result<u8, String> {
        let setup = connection.setup();
        let count = setup
            .max_keycode
            .saturating_sub(setup.min_keycode)
            .saturating_add(1);
        let mapping = connection
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|error| format!("读取 X11 键盘映射失败：{error}"))?
            .reply()
            .map_err(|error| format!("读取 X11 键盘映射失败：{error}"))?;
        let per_key = usize::from(mapping.keysyms_per_keycode);
        if per_key == 0 {
            return Err("X11 键盘映射为空".into());
        }
        mapping
            .keysyms
            .chunks(per_key)
            .position(|symbols| symbols.contains(&keysym))
            .and_then(|index| u8::try_from(index).ok())
            .and_then(|index| setup.min_keycode.checked_add(index))
            .ok_or_else(|| format!("当前 X11 键盘布局不包含 keysym 0x{keysym:x}"))
    }

    fn modifier_keysym(modifier: &str) -> Result<u32, String> {
        Ok(match modifier.to_ascii_lowercase().as_str() {
            "shift" => 0xffe1,
            "control" | "ctrl" => 0xffe3,
            "alt" | "option" => 0xffe9,
            "meta" | "command" | "cmd" => 0xffeb,
            other => return Err(format!("不支持的修饰键：{other}")),
        })
    }

    fn key_keysym(key: &str) -> Result<u32, String> {
        Ok(match key.to_ascii_lowercase().as_str() {
            "enter" | "return" => 0xff0d,
            "tab" => 0xff09,
            "space" => 0x20,
            "backspace" => 0xff08,
            "delete" => 0xffff,
            "escape" | "esc" => 0xff1b,
            "left" | "arrowleft" => 0xff51,
            "up" | "arrowup" => 0xff52,
            "right" | "arrowright" => 0xff53,
            "down" | "arrowdown" => 0xff54,
            "home" => 0xff50,
            "end" => 0xff57,
            "pageup" => 0xff55,
            "pagedown" => 0xff56,
            "insert" => 0xff63,
            value if value.starts_with('f') && value[1..].parse::<u32>().is_ok() => {
                let number = value[1..].parse::<u32>().unwrap_or_default();
                if !(1..=24).contains(&number) {
                    return Err(format!("不支持的按键：{key}"));
                }
                0xffbd + number
            }
            value if value.chars().count() == 1 => value.chars().next().unwrap() as u32,
            other => return Err(format!("不支持的按键：{other}")),
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
compile_error!("Computer Use has no backend for this target OS");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_frames_are_rejected() {
        let controller = ComputerController {
            last_frame: Some(StoredFrame {
                meta: FrameMeta {
                    frame_id: "current".into(),
                    display_id: "d".into(),
                    image_width: 100,
                    image_height: 50,
                    origin_x: -20.0,
                    origin_y: 10.0,
                    logical_width: 200.0,
                    logical_height: 100.0,
                    scale_factor: 0.5,
                    captured_at_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                    target_name: None,
                },
                visual_signature: vec![0; 32 * 18],
                foreground_target: None,
            }),
        };
        assert!(controller.resolve_point("old", 10.0, 10.0).is_err());
        assert_eq!(
            controller.resolve_point("current", 50.0, 25.0).unwrap(),
            (80.0, 60.0)
        );
        assert!(controller.resolve_point("current", 101.0, 0.0).is_err());
        // Width/height are dimensions, not inclusive pixel coordinates. Their
        // transformed edge can belong to a different monitor or application.
        assert!(controller.resolve_point("current", 100.0, 0.0).is_err());
        assert!(controller.resolve_point("current", 0.0, 50.0).is_err());
        assert!(controller.resolve_point("current", f64::NAN, 0.0).is_err());
        assert!(controller.resolve_point("current", 99.0, 49.0).is_ok());
    }

    #[test]
    fn visual_guard_allows_minor_noise_but_rejects_replaced_screen() {
        let baseline = vec![100_u8; 32 * 18];
        let mut minor = baseline.clone();
        minor[..20].fill(130);
        assert!(visual_signatures_match(&baseline, &minor));
        let replaced = vec![220_u8; 32 * 18];
        assert!(!visual_signatures_match(&baseline, &replaced));
    }
}
