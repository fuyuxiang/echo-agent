//! Isolated window capture, with a guarded OS-cropped video fallback for a
//! fully visible foreground window. Never activates an app or captures a display.
use super::*;
use base64::Engine;

fn dictionary_number(dictionary: Raw, key: &str) -> Option<f64> {
    let value = unsafe { CFDictionaryGetValue(dictionary, cf(key).0) };
    let mut result = 0f64;
    (!value.is_null()
        && unsafe {
            CFGetTypeID(value) == CFNumberGetTypeID()
                && CFNumberGetValue(value, 6, &mut result as *mut _ as *mut c_void)
        })
    .then_some(result)
}

pub(super) fn bounds(element: &Element) -> Result<[f64; 4], String> {
    let p = attr(element.raw.0, "AXPosition")?.ok_or("window_bounds_unavailable")?;
    let s = attr(element.raw.0, "AXSize")?.ok_or("window_bounds_unavailable")?;
    if unsafe { CFGetTypeID(p.0) != AXValueGetTypeID() || CFGetTypeID(s.0) != AXValueGetTypeID() } {
        return Err("window_bounds_unavailable".into());
    }
    let mut origin = Pair::default();
    let mut size = Pair::default();
    if !unsafe {
        AXValueGetValue(p.0, 1, (&mut origin as *mut Pair).cast())
            && AXValueGetValue(s.0, 2, (&mut size as *mut Pair).cast())
    } {
        return Err("window_bounds_unavailable".into());
    }
    Ok([origin.a, origin.b, size.a, size.b])
}
pub(super) fn window_id(backend: &Backend, element: &Element) -> Result<u32, String> {
    use core_graphics::window::*;
    backend.alive(element)?;
    let bounds = bounds(element)?;
    let windows = owned(unsafe {
        CGWindowListCopyWindowInfo(kCGWindowListExcludeDesktopElements, 0).cast()
    })?;
    let pids = backend.process_ids(element);
    let mut candidates = Vec::new();
    for window in array(&windows, 4096) {
        if unsafe { CFGetTypeID(window.0) != CFDictionaryGetTypeID() } {
            continue;
        }
        if !dictionary_number(window.0, "kCGWindowOwnerPID")
            .is_some_and(|pid| pids.iter().any(|p| f64::from(*p) == pid))
        {
            continue;
        }
        let rect = unsafe { CFDictionaryGetValue(window.0, cf("kCGWindowBounds").0) };
        if rect.is_null() || unsafe { CFGetTypeID(rect) != CFDictionaryGetTypeID() } {
            continue;
        }
        let actual = [
            dictionary_number(rect, "X"),
            dictionary_number(rect, "Y"),
            dictionary_number(rect, "Width"),
            dictionary_number(rect, "Height"),
        ];
        if actual
            .iter()
            .zip(bounds)
            .all(|(a, b)| a.is_some_and(|a| (a - b).abs() <= 2.0))
        {
            if let Some(id) = dictionary_number(window.0, "kCGWindowNumber") {
                candidates.push(id as u32);
            }
        }
    }
    if candidates.len() != 1 {
        return Err("ambiguous_window_capture: unique native window identity not available; use accessibility observation".into());
    }
    Ok(candidates[0])
}

pub(super) fn verify_point(
    backend: &Backend,
    element: &Element,
    x: f64,
    y: f64,
) -> Result<bool, String> {
    use core_graphics::window::*;
    if !x.is_finite() || !y.is_finite() {
        return Err("invalid_argument: invalid visual point".into());
    }
    let expected = window_id(backend, element)?;
    let windows = owned(unsafe {
        CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            0,
        )
        .cast()
    })?;
    let parent_bounds = bounds(element)?;
    let parent_layer = array(&windows, 4096)
        .into_iter()
        .find(|w| dictionary_number(w.0, "kCGWindowNumber") == Some(f64::from(expected)))
        .and_then(|w| dictionary_number(w.0, "kCGWindowLayer"))
        .ok_or("target_changed: native window layer unavailable")?;
    for window in array(&windows, 4096) {
        if dictionary_number(window.0, "kCGWindowAlpha").is_some_and(|a| a <= 0.) {
            continue;
        }
        let bounds = unsafe { CFDictionaryGetValue(window.0, cf("kCGWindowBounds").0) };
        if bounds.is_null() || unsafe { CFGetTypeID(bounds) != CFDictionaryGetTypeID() } {
            continue;
        }
        let rect = [
            dictionary_number(bounds, "X"),
            dictionary_number(bounds, "Y"),
            dictionary_number(bounds, "Width"),
            dictionary_number(bounds, "Height"),
        ];
        if let [Some(bx), Some(by), Some(w), Some(h)] = rect {
            if x >= bx && y >= by && x < bx + w && y < by + h {
                if dictionary_number(window.0, "kCGWindowNumber") == Some(f64::from(expected))
                    && dictionary_number(window.0, "kCGWindowOwnerPID")
                        == Some(f64::from(element.app.processIdentifier()))
                {
                    return Ok(false);
                }
                // The Dock owns a full-display transparent window. Check the
                // actual overlay pixel, not just its enclosing rectangle.
                let foreign_id = dictionary_number(window.0, "kCGWindowNumber").map(|n| n as u32);
                if let Some(id) = foreign_id {
                    if verified_transient(
                        backend,
                        element,
                        window.0,
                        id,
                        parent_bounds,
                        parent_layer,
                    )? {
                        return Ok(true);
                    }
                }
                if let Some(id) = foreign_id
                    .filter(|_| dictionary_number(window.0, "kCGWindowSharingState") == Some(1.))
                {
                    use core_graphics::{
                        color_space::CGColorSpace,
                        context::CGContext,
                        geometry::{CGPoint, CGRect, CGSize},
                    };
                    if let Some(image) = create_image(
                        CGRect::new(&CGPoint::new(x, y), &CGSize::new(1., 1.)),
                        kCGWindowListOptionIncludingWindow,
                        id,
                        kCGWindowImageBoundsIgnoreFraming,
                    ) {
                        let mut pixel = CGContext::create_bitmap_context(
                            None,
                            1,
                            1,
                            8,
                            4,
                            &CGColorSpace::create_device_rgb(),
                            1 | (4 << 12),
                        );
                        pixel.draw_image(
                            CGRect::new(&CGPoint::new(0., 0.), &CGSize::new(1., 1.)),
                            &image,
                        );
                        if pixel.data()[3] == 0 {
                            continue;
                        }
                    }
                }
                return Err("target_changed: another native window covers this point; capture the requested window again".into());
            }
        }
    }
    Err("target_changed: no trusted on-screen window at the requested point".into())
}

/// A non-modal, non-main AX dialog/floating window contained in this window is
/// part of its visible UI. PID alone never authorizes an independent document.
fn verified_transient(
    backend: &Backend,
    parent: &Element,
    info: Raw,
    id: u32,
    parent_bounds: [f64; 4],
    parent_layer: f64,
) -> Result<bool, String> {
    if dictionary_number(info, "kCGWindowOwnerPID")
        != Some(f64::from(parent.app.processIdentifier()))
        || !dictionary_number(info, "kCGWindowLayer").is_some_and(|layer| layer > parent_layer)
    {
        return Ok(false);
    }
    let rect = unsafe { CFDictionaryGetValue(info, cf("kCGWindowBounds").0) };
    if rect.is_null() || unsafe { CFGetTypeID(rect) != CFDictionaryGetTypeID() } {
        return Ok(false);
    }
    let [Some(x), Some(y), Some(w), Some(h)] = [
        dictionary_number(rect, "X"),
        dictionary_number(rect, "Y"),
        dictionary_number(rect, "Width"),
        dictionary_number(rect, "Height"),
    ] else {
        return Ok(false);
    };
    if x < parent_bounds[0]
        || y < parent_bounds[1]
        || x + w > parent_bounds[0] + parent_bounds[2]
        || y + h > parent_bounds[1] + parent_bounds[3]
    {
        return Ok(false);
    }
    let app = owned(unsafe { AXUIElementCreateApplication(parent.app.processIdentifier()) })?;
    let Some(windows) = attr(app.0, "AXWindows")? else {
        return Ok(false);
    };
    let start = Instant::now();
    for raw in array(&windows, 16) {
        if start.elapsed() > Duration::from_secs(2) {
            break;
        }
        let candidate = Element::for_host(raw, parent.app.clone());
        if !bounds(&candidate).is_ok_and(|b| {
            b.iter()
                .zip([x, y, w, h])
                .all(|(a, b)| (a - b).abs() <= 0.5)
        }) {
            continue;
        }
        if !matches!(
            string(candidate.raw.0, "AXSubrole", 128)?.as_str(),
            "AXDialog" | "AXFloatingWindow" | "AXSystemFloatingWindow"
        ) || boolean(candidate.raw.0, "AXModal")? != Some(false)
            || boolean(candidate.raw.0, "AXMain")? != Some(false)
        {
            return Ok(false);
        }
        return Ok(window_id(backend, &candidate)? == id);
    }
    Ok(false)
}

fn verify_foreground_region(
    backend: &Backend,
    element: &Element,
    expected: u32,
    expected_bounds: [f64; 4],
) -> Result<Vec<u32>, String> {
    use core_graphics::{
        color_space::CGColorSpace,
        context::CGContext,
        geometry::{CGPoint, CGRect, CGSize},
        window::*,
    };
    let front = backend.foreground_window()?;
    if !backend.same_element(element, &front) {
        return Err("foreground_required: this window requires foreground video observation; explicitly activate the requested window".into());
    }
    if window_id(backend, element)? != expected
        || bounds(element)?
            .iter()
            .zip(expected_bounds)
            .any(|(a, b)| (a - b).abs() > 0.5)
    {
        return Err("target_changed: window moved during foreground video capture".into());
    }
    let windows = owned(unsafe {
        CGWindowListCopyWindowInfo(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            0,
        )
        .cast()
    })?;
    let parent_layer = array(&windows, 4096)
        .into_iter()
        .find(|w| dictionary_number(w.0, "kCGWindowNumber") == Some(f64::from(expected)))
        .and_then(|w| dictionary_number(w.0, "kCGWindowLayer"))
        .ok_or("target_changed: native window layer unavailable")?;
    let mut transients = vec![];
    for window in array(&windows, 4096) {
        if dictionary_number(window.0, "kCGWindowAlpha").is_some_and(|a| a <= 0.) {
            continue;
        }
        let rect = unsafe { CFDictionaryGetValue(window.0, cf("kCGWindowBounds").0) };
        if rect.is_null() || unsafe { CFGetTypeID(rect) != CFDictionaryGetTypeID() } {
            return Err("capture_unavailable: unknown foreground window geometry".into());
        }
        let actual = [
            dictionary_number(rect, "X"),
            dictionary_number(rect, "Y"),
            dictionary_number(rect, "Width"),
            dictionary_number(rect, "Height"),
        ];
        let [Some(x), Some(y), Some(w), Some(h)] = actual else {
            return Err("capture_unavailable: unknown foreground window geometry".into());
        };
        let left = x.max(expected_bounds[0]);
        let top = y.max(expected_bounds[1]);
        let right = (x + w).min(expected_bounds[0] + expected_bounds[2]);
        let bottom = (y + h).min(expected_bounds[1] + expected_bounds[3]);
        if right <= left || bottom <= top {
            continue;
        }
        let id = dictionary_number(window.0, "kCGWindowNumber")
            .ok_or("capture_unavailable: unknown overlay identity")? as u32;
        if id == expected
            && dictionary_number(window.0, "kCGWindowOwnerPID")
                == Some(f64::from(element.app.processIdentifier()))
        {
            return Ok(transients);
        }
        if verified_transient(
            backend,
            element,
            window.0,
            id,
            expected_bounds,
            parent_layer,
        )? {
            transients.push(id);
            continue;
        }
        // An enclosing rectangle alone does not mean the Dock's transparent
        // overlay covers the app. Permit it only after EVERY overlap pixel is
        // proven transparent; restricted/unknown/visible overlays fail closed.
        if dictionary_number(window.0, "kCGWindowSharingState") == Some(1.) {
            if let Some(image) = create_image(
                CGRect::new(
                    &CGPoint::new(left, top),
                    &CGSize::new(right - left, bottom - top),
                ),
                kCGWindowListOptionIncludingWindow,
                id,
                kCGWindowImageBoundsIgnoreFraming,
            ) {
                let (width, height) = (image.width(), image.height());
                if width > 0
                    && height > 0
                    && width.checked_mul(height).is_some_and(|n| n <= 16_000_000)
                {
                    let mut context = CGContext::create_bitmap_context(
                        None,
                        width,
                        height,
                        8,
                        width * 4,
                        &CGColorSpace::create_device_rgb(),
                        1 | (4 << 12),
                    );
                    context.draw_image(
                        CGRect::new(
                            &CGPoint::new(0., 0.),
                            &CGSize::new(width as f64, height as f64),
                        ),
                        &image,
                    );
                    if context.data().as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
                        continue;
                    }
                }
            }
        }
        return Err("target_changed: another window overlaps the foreground video region; expose the requested window before capturing".into());
    }
    Err("target_changed: requested video window is not on screen".into())
}

pub(super) fn capture(backend: &Backend, element: &Element) -> Result<Value, String> {
    let id = window_id(backend, element)?;
    let node = backend.inspect(element)?;
    let bounds = node
        .bounds
        .ok_or("unsupported_action: window bounds unavailable")?;
    if bounds.iter().any(|n| !n.is_finite()) || bounds[2] <= 0. || bounds[3] <= 0. {
        return Err("capture_unavailable: invalid window geometry".into());
    }
    let scale = (1600. / bounds[2].max(bounds[3])).min(2.);
    let bytes = super::super::macos_screencapture::capture(
        id,
        element.app.processIdentifier(),
        (bounds[2] * scale).round().max(1.) as usize,
        (bounds[3] * scale).round().max(1.) as usize,
    )?;
    let mut original =
        image::load_from_memory(&bytes).map_err(|_| "capture_unavailable: invalid system image")?;
    let mut mode = "screencapturekit_window_only";
    if !original.to_rgba8().pixels().any(|p| p[3] > 0)
        || (cfg!(test) && std::env::var("ECHO_NATIVE_FORCE_WINDOW_VIDEO").as_deref() == Ok("1"))
    {
        let before = verify_foreground_region(backend, element, id, bounds)?;
        let bytes = super::super::macos_video_capture::capture(
            bounds,
            (bounds[2] * scale).round().max(1.) as usize,
        )?;
        if before != verify_foreground_region(backend, element, id, bounds)? {
            return Err("target_changed: native transient windows changed during capture".into());
        }
        original = image::load_from_memory(&bytes)
            .map_err(|_| "capture_unavailable: invalid foreground video image")?;
        mode = "avfoundation_foreground_window_region";
    }
    bounded_image(backend, element, original, bounds, mode)
}

pub(super) fn bounded_image(
    backend: &Backend,
    element: &Element,
    original: image::DynamicImage,
    bounds: [f64; 4],
    mode: &str,
) -> Result<Value, String> {
    if !original.to_rgba8().pixels().any(|p| p[3] > 0) {
        return Err("capture_unavailable: no visible pixels".into());
    }
    for max in [1600, 1000, 640] {
        let resized = original.thumbnail(max, max);
        let mut png = std::io::Cursor::new(Vec::new());
        resized
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        if png.get_ref().len() < 1_000_000 {
            backend.alive(element)?;
            return Ok(
                json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner()),"imageWidth":resized.width(),"imageHeight":resized.height(),"windowBounds":bounds,"targetName":backend.inspect(element)?.name,"coordinateInput":false,"captureMode":mode}),
            );
        }
    }
    Err("capture_unavailable: image exceeds bounded helper transport".into())
}
