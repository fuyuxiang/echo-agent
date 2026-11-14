//! Public ScreenCaptureKit window capture (macOS 14+). No display fallback.
use block2::RcBlock;
use objc2::{
    class,
    encode::{Encoding, RefEncode},
    msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, Bool},
};
use objc2_foundation::NSArray;
use std::{
    ffi::c_void,
    sync::mpsc,
    time::{Duration, Instant},
};

#[link(name = "ScreenCaptureKit", kind = "framework")]
unsafe extern "C" {}
#[repr(C)]
struct Image {
    _private: [u8; 0],
}
unsafe impl RefEncode for Image {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGImage", &[]));
}
pub(super) fn failure(error: *mut AnyObject) -> String {
    let code: isize = if error.is_null() {
        0
    } else {
        unsafe { msg_send![error, code] }
    };
    if code == -3801 {
        "permission_denied: ScreenCaptureKit access was declined".into()
    } else {
        format!("capture_unavailable: ScreenCaptureKit failed (code={code})")
    }
}
pub(super) fn capture(id: u32, pid: i32, width: usize, height: usize) -> Result<Vec<u8>, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let content_block = RcBlock::new(move |content: *mut AnyObject, error: *mut AnyObject| {
        objc2::rc::autoreleasepool(|_| unsafe {
            if content.is_null() || !error.is_null() {
                let _ = tx.send(Err(failure(error)));
                return;
            }
            let windows: Retained<NSArray<AnyObject>> = msg_send![content, windows];
            let selected = windows.iter().find(|w| {
                let actual: u32 = msg_send![&**w, windowID];
                if actual != id {
                    return false;
                }
                let app: Option<Retained<AnyObject>> = msg_send![&**w, owningApplication];
                app.is_some_and(|app| {
                    let actual: i32 = msg_send![&*app, processID];
                    actual == pid
                })
            });
            let Some(window) = selected else {
                let _ = tx.send(Err(
                    "target_changed: requested window is not shareable".into()
                ));
                return;
            };
            let allocated: Allocated<AnyObject> = msg_send![class!(SCContentFilter), alloc];
            let filter: Retained<AnyObject> =
                msg_send![allocated,initWithDesktopIndependentWindow:&*window];
            let config: Retained<AnyObject> = msg_send![class!(SCStreamConfiguration), new];
            let _: () = msg_send![&*config,setWidth:width];
            let _: () = msg_send![&*config,setHeight:height];
            let _: () = msg_send![&*config,setShowsCursor:Bool::NO];
            let _: () = msg_send![&*config,setIgnoreShadowsSingleWindow:Bool::YES];
            let tx = tx.clone();
            let image_block = RcBlock::new(move |image: *const Image, error: *mut AnyObject| {
                let result = objc2::rc::autoreleasepool(|_| {
                    if image.is_null() || !error.is_null() {
                        return Err(failure(error));
                    }
                    let allocated: Allocated<AnyObject> =
                        msg_send![class!(NSBitmapImageRep), alloc];
                    let rep: Option<Retained<AnyObject>> =
                        msg_send![allocated,initWithCGImage:image];
                    let rep = rep.ok_or("capture_unavailable: image conversion unavailable")?;
                    let properties: Retained<AnyObject> =
                        msg_send![class!(NSDictionary), dictionary];
                    let data: Option<Retained<AnyObject>> =
                        msg_send![&*rep,representationUsingType:4usize,properties:&*properties];
                    let data = data.ok_or("capture_unavailable: PNG encoding unavailable")?;
                    let length: usize = msg_send![&*data, length];
                    let bytes: *const c_void = msg_send![&*data, bytes];
                    if length == 0 || length > 16_000_000 || bytes.is_null() {
                        return Err("capture_unavailable: image exceeded bounds".into());
                    }
                    Ok(std::slice::from_raw_parts(bytes.cast::<u8>(), length).to_vec())
                });
                let _ = tx.send(result);
            });
            let _: () = msg_send![class!(SCScreenshotManager),captureImageWithFilter:&*filter,configuration:&*config,completionHandler:&*image_block];
        });
    });
    unsafe {
        let _: () = msg_send![class!(SCShareableContent),getShareableContentExcludingDesktopWindows:Bool::YES,onScreenWindowsOnly:Bool::NO,completionHandler:&*content_block];
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        match rx.try_recv() {
            Ok(value) => return value,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("capture_unavailable: capture callback disconnected".into())
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err("native_timeout: ScreenCaptureKit exceeded capture deadline".into());
        }
        // CLI helpers have no NSApplication event loop. Pump a bounded native
        // run loop while callbacks complete; the parent can still kill on Stop.
        super::macos::pump_capture_events();
        std::thread::sleep(Duration::from_millis(5));
    }
}
