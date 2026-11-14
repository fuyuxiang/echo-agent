//! In-process system Vision OCR, inside the cancellable native helper deadline.
use super::visual_text::Line;
use objc2::{
    class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyObject, Bool},
};
use objc2_foundation::{NSArray, NSString};

#[link(name = "Vision", kind = "framework")]
unsafe extern "C" {}

pub(super) fn recognize(png: &[u8]) -> Result<Vec<Line>, String> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let data: Retained<AnyObject> = msg_send![class!(NSData), dataWithBytes:png.as_ptr().cast::<std::ffi::c_void>(), length:png.len()];
        let options: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        let allocated: Allocated<AnyObject> = msg_send![class!(VNImageRequestHandler), alloc];
        let handler: Option<Retained<AnyObject>> =
            msg_send![allocated, initWithData:&*data, options:&*options];
        let handler = handler.ok_or("visual_text_unavailable: Vision image handler unavailable")?;
        let request: Retained<AnyObject> = msg_send![class!(VNRecognizeTextRequest), new];
        let zh = NSString::from_str("zh-Hans");
        let en = NSString::from_str("en-US");
        let languages = NSArray::from_slice(&[&*zh, &*en]);
        let _: () = msg_send![&*request, setRecognitionLevel:0isize];
        let _: () = msg_send![&*request, setRecognitionLanguages:&*languages];
        let _: () = msg_send![&*request, setUsesLanguageCorrection:Bool::NO];
        let requests = NSArray::from_slice(&[&*request]);
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let ok: Bool = msg_send![&*handler, performRequests:&*requests, error:&mut error];
        if !ok.as_bool() {
            let code: isize = if error.is_null() {
                0
            } else {
                msg_send![error, code]
            };
            return Err(format!(
                "visual_text_unavailable: system OCR request failed (code={code})"
            ));
        }
        let results: Option<Retained<NSArray<AnyObject>>> = msg_send![&*request, results];
        let mut lines = vec![];
        if let Some(results) = results {
            if results.len() > 256 {
                return Err("visual_text_truncated: narrow the requested region".into());
            }
            for observation in results.iter() {
                let candidates: Retained<NSArray<AnyObject>> =
                    msg_send![&*observation, topCandidates:1usize];
                if candidates.is_empty() {
                    continue;
                }
                let candidate = candidates.objectAtIndex(0);
                let text: Retained<NSString> = msg_send![&*candidate, string];
                let confidence: f32 = msg_send![&*candidate, confidence];
                lines.push(Line {
                    text: text.to_string(),
                    confidence,
                });
            }
        }
        Ok(lines)
    })
}
