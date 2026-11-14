//! One-frame public AVFoundation fallback, cropped by the OS to a verified
//! foreground window. The caller guards identity, bounds and occlusion twice.
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, define_class, extern_protocol, msg_send, AnyThread, DefinedClass};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize};
use std::ffi::{c_char, c_void};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[repr(C)]
struct SampleBuffer {
    _private: [u8; 0],
}
unsafe impl RefEncode for SampleBuffer {
    const ENCODING_REF: Encoding =
        Encoding::Pointer(&Encoding::Struct("opaqueCMSampleBuffer", &[]));
}
#[repr(C)]
#[derive(Clone, Copy)]
struct MediaTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}
unsafe impl Encode for MediaTime {
    const ENCODING: Encoding = Encoding::Struct(
        "?",
        &[i64::ENCODING, i32::ENCODING, u32::ENCODING, i64::ENCODING],
    );
}
unsafe impl RefEncode for MediaTime {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetImageBuffer(buffer: *const SampleBuffer) -> *mut c_void;
}
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetBytesPerRow(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetBaseAddress(buffer: *mut c_void) -> *const u8;
    fn CVPixelBufferGetPixelFormatType(buffer: *mut c_void) -> u32;
}
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut AnyObject;
}
extern_protocol!(
    unsafe trait AVCaptureVideoDataOutputSampleBufferDelegate: NSObjectProtocol {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        #[optional]
        unsafe fn output(
            &self,
            output: &AnyObject,
            sample: *const SampleBuffer,
            connection: &AnyObject,
        );
    }
);
struct FrameState {
    accept: AtomicBool,
    delivered: AtomicBool,
    tx: mpsc::SyncSender<Vec<u8>>,
}
define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = FrameState]
    struct EchoWindowFrameOutput;
    unsafe impl NSObjectProtocol for EchoWindowFrameOutput {}
    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for EchoWindowFrameOutput {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        unsafe fn output(
            &self,
            _output: &AnyObject,
            sample: *const SampleBuffer,
            _connection: &AnyObject,
        ) {
            let state = self.ivars();
            if !state.accept.load(Ordering::Acquire) || state.delivered.load(Ordering::Acquire) {
                return;
            }
            // The sample is retained by AVFoundation for this callback. Copy
            // only its locked BGRA buffer; no platform pointers escape the call.
            if let Some(png) = unsafe { sample_png(sample) } {
                if !state.delivered.swap(true, Ordering::AcqRel) {
                    let _ = state.tx.try_send(png);
                }
            }
        }
    }
);
unsafe fn sample_png(sample: *const SampleBuffer) -> Option<Vec<u8>> {
    if sample.is_null() {
        return None;
    }
    let buffer = unsafe { CMSampleBufferGetImageBuffer(sample) };
    if buffer.is_null()
        || unsafe { CVPixelBufferGetPixelFormatType(buffer) } != u32::from_be_bytes(*b"BGRA")
    {
        return None;
    }
    if unsafe { CVPixelBufferLockBaseAddress(buffer, 1) } != 0 {
        return None;
    }
    struct Unlock(*mut c_void);
    impl Drop for Unlock {
        fn drop(&mut self) {
            unsafe {
                CVPixelBufferUnlockBaseAddress(self.0, 1);
            }
        }
    }
    let _unlock = Unlock(buffer);
    let width = unsafe { CVPixelBufferGetWidth(buffer) };
    let height = unsafe { CVPixelBufferGetHeight(buffer) };
    let stride = unsafe { CVPixelBufferGetBytesPerRow(buffer) };
    let base = unsafe { CVPixelBufferGetBaseAddress(buffer) };
    if width == 0
        || height == 0
        || width > 3200
        || height > 3200
        || stride < width * 4
        || stride > 65536
        || base.is_null()
    {
        return None;
    }
    let mut image = image::RgbaImage::new(width as u32, height as u32);
    for y in 0..height {
        let row = unsafe { std::slice::from_raw_parts(base.add(y * stride), width * 4) };
        let bytes: &mut [u8] = image.as_mut();
        let target = &mut bytes[y * width * 4..(y + 1) * width * 4];
        for (rgba, bgra) in target.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
            rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
        }
    }
    if !has_visible_detail(&image) {
        return None;
    }
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut png, image::ImageFormat::Png)
        .ok()?;
    Some(png.into_inner())
}

fn has_visible_detail(image: &image::RgbaImage) -> bool {
    let mut first = None;
    for pixel in image.pixels().filter(|p| p[3] > 0) {
        match first {
            None => first = Some(*pixel),
            Some(value) if value != *pixel => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blank_or_transparent_video_cannot_authorize_visual_input() {
        assert!(!has_visible_detail(&image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([0, 0, 0, 255])
        )));
        let mut transparent = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
        transparent.put_pixel(1, 1, image::Rgba([255, 0, 0, 0]));
        assert!(!has_visible_detail(&transparent));
        transparent.put_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        assert!(!has_visible_detail(&transparent));
        transparent.put_pixel(2, 1, image::Rgba([255, 255, 255, 255]));
        assert!(has_visible_detail(&transparent));
    }
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: *const AnyObject;
}
unsafe extern "C" {
    fn dispatch_sync_f(
        queue: *const AnyObject,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}
extern "C" fn drained(_: *mut c_void) {}

/// Returns only the system-cropped foreground rectangle. No audio or movie file.
/// Called in a short-lived helper: Stop kills/reaps even a blocked startRunning.
pub(super) fn capture(bounds: [f64; 4], width: usize) -> Result<Vec<u8>, String> {
    use core_graphics::display::CGDisplay;
    let displays =
        CGDisplay::active_displays().map_err(|_| "capture_unavailable: displays unavailable")?;
    let (id, display) = displays
        .into_iter()
        .find_map(|id| {
            let display = CGDisplay::new(id);
            let frame = display.bounds();
            (bounds[0] >= frame.origin.x
                && bounds[1] >= frame.origin.y
                && bounds[0] + bounds[2] <= frame.origin.x + frame.size.width
                && bounds[1] + bounds[3] <= frame.origin.y + frame.size.height)
                .then_some((id, display))
        })
        .ok_or("capture_unavailable: foreground video window must fit on one display")?;
    let frame = display.bounds();
    let crop = NSRect::new(
        NSPoint::new(
            bounds[0] - frame.origin.x,
            frame.size.height - (bounds[1] - frame.origin.y) - bounds[3],
        ),
        NSSize::new(bounds[2], bounds[3]),
    );
    let pixels_per_point = display.pixels_wide() as f64 / frame.size.width;
    let scale = (width as f64 / bounds[2] / pixels_per_point).min(1.);
    unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class!(AVCaptureScreenInput), alloc];
        let input: Option<Retained<AnyObject>> = msg_send![allocated,initWithDisplayID:id];
        let input = input.ok_or("capture_unavailable: foreground video input unavailable")?;
        let _: () = msg_send![&*input,setCropRect:crop];
        let _: () = msg_send![&*input,setScaleFactor:scale];
        let _: () = msg_send![&*input,setCapturesCursor:Bool::NO];
        let _: () = msg_send![&*input,setCapturesMouseClicks:Bool::NO];
        let _: () =
            msg_send![&*input,setMinFrameDuration:MediaTime{value:1,timescale:4,flags:1,epoch:0}];
        let session: Retained<AnyObject> = msg_send![class!(AVCaptureSession), new];
        let video: Retained<AnyObject> = msg_send![class!(AVCaptureVideoDataOutput), new];
        let pixel_format: Retained<AnyObject> =
            msg_send![class!(NSNumber),numberWithUnsignedInt:u32::from_be_bytes(*b"BGRA")];
        let settings: Retained<AnyObject> = msg_send![class!(NSDictionary),dictionaryWithObject:&*pixel_format,forKey:kCVPixelBufferPixelFormatTypeKey];
        let _: () = msg_send![&*video,setVideoSettings:&*settings];
        let _: () = msg_send![&*video,setAlwaysDiscardsLateVideoFrames:Bool::YES];
        let (tx, rx) = mpsc::sync_channel(1);
        let allocated = EchoWindowFrameOutput::alloc().set_ivars(FrameState {
            accept: AtomicBool::new(true),
            delivered: AtomicBool::new(false),
            tx,
        });
        let output: Retained<EchoWindowFrameOutput> = msg_send![super(allocated), init];
        let queue = Retained::from_raw(dispatch_queue_create(
            c"com.echoagent.foreground-frame".as_ptr(),
            std::ptr::null(),
        ))
        .ok_or("capture_unavailable: capture queue unavailable")?;
        let can_input: Bool = msg_send![&*session,canAddInput:&*input];
        let can_output: Bool = msg_send![&*session,canAddOutput:&*video];
        if !can_input.as_bool() || !can_output.as_bool() {
            return Err("capture_unavailable: foreground video session unavailable".into());
        }
        let _: () = msg_send![&*video,setSampleBufferDelegate:&*output,queue:&*queue];
        let _: () = msg_send![&*session, beginConfiguration];
        let _: () = msg_send![&*session,addInput:&*input];
        let _: () = msg_send![&*session,addOutput:&*video];
        let _: () = msg_send![&*session, commitConfiguration];
        let _: () = msg_send![&*session, startRunning];
        let deadline = Instant::now() + Duration::from_secs(6);
        let result = loop {
            match rx.try_recv() {
                Ok(png) => break Ok(png),
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err("capture_unavailable: foreground frame callback disconnected".into())
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if Instant::now() >= deadline {
                break Err("capture_unavailable: no valid foreground video frame".into());
            }
            super::macos::pump_capture_events();
            std::thread::sleep(Duration::from_millis(5));
        };
        output.ivars().accept.store(false, Ordering::Release);
        // stopRunning is synchronous; detach and drain callbacks before dropping
        // delegate or queue. The parent helper has a separate hard kill deadline.
        let _: () = msg_send![&*session, stopRunning];
        let _: () = msg_send![&*video,setSampleBufferDelegate:std::ptr::null::<AnyObject>(),queue:std::ptr::null::<AnyObject>()];
        dispatch_sync_f(&*queue, std::ptr::null_mut(), drained);
        result
    }
}
