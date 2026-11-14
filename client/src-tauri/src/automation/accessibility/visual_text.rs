//! Positive, bounded OCR assertions over fresh, window-bound image regions.
//! Text is observed by the OS engine; expectations never bias recognition.
use base64::Engine;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct Line {
    pub text: String,
    pub confidence: f32,
}
fn normalized(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
fn checks(args: &Value) -> Result<Vec<Value>, String> {
    let mut value = args["checks"].clone();
    if let Some(text) = value.as_str() {
        if text.len() > 40000 || !text.trim_start().starts_with('[') {
            return Err("invalid_argument: checks must be an array".into());
        }
        value = serde_json::from_str(text).map_err(|_| "invalid_argument: invalid checks JSON")?;
    }
    if let Some(object) = value.as_object() {
        if object.len() == 1 && object.contains_key("item") {
            value = object["item"].clone();
            if value.is_object() {
                value = json!([value]);
            }
        }
    }
    let values = value
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 8)
        .ok_or("invalid_argument: provide 1..8 visual checks")?;
    for check in values {
        let text = check["text"]
            .as_str()
            .filter(|s| !normalized(s).is_empty() && s.len() <= 4000)
            .ok_or("invalid_argument: visual text must contain 1..4000 bytes")?;
        let _ = text;
        region(check)?;
    }
    Ok(values.clone())
}
fn region(check: &Value) -> Result<[f64; 4], String> {
    let read = |key| {
        super::super::arguments::number(&check["region"][key])
            .and_then(|n| n.as_f64())
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("invalid_argument: region.{key} must be a number"))
    };
    let r = [read("x")?, read("y")?, read("width")?, read("height")?];
    if r[0] < 0. || r[1] < 0. || r[2] <= 0. || r[3] <= 0. || r[0] + r[2] > 1. || r[1] + r[3] > 1. {
        return Err(
            "invalid_argument: region is normalized top-left x/y/width/height within 0..1".into(),
        );
    }
    Ok(r)
}
pub(super) fn validate(args: &Value) -> Result<(), String> {
    checks(args).map(|_| ())
}

pub(super) fn verify(
    capture: &Value,
    args: &Value,
    token: &CancellationToken,
    mut recognize: impl FnMut(&[u8]) -> Result<Vec<Line>, String>,
) -> Result<Value, String> {
    let checks = checks(args)?;
    let data = capture["data"]
        .as_str()
        .filter(|s| s.len() <= 2_000_000)
        .ok_or("capture_unavailable: bounded PNG required")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "capture_unavailable: invalid image encoding")?;
    let image =
        image::load_from_memory(&bytes).map_err(|_| "capture_unavailable: invalid image")?;
    if image.width() == 0
        || image.height() == 0
        || u64::from(image.width()) * u64::from(image.height()) > 32_000_000
    {
        return Err("capture_unavailable: image dimensions out of bounds".into());
    }
    let mut observations = vec![];
    for check in checks {
        if token.is_cancelled() {
            return Err("cancelled: visual verification cancelled".into());
        }
        let r = region(&check)?;
        let x = (r[0] * f64::from(image.width())).floor() as u32;
        let y = (r[1] * f64::from(image.height())).floor() as u32;
        let width = ((r[2] * f64::from(image.width())).ceil() as u32).min(image.width() - x);
        let height = ((r[3] * f64::from(image.height())).ceil() as u32).min(image.height() - y);
        if width < 4 || height < 4 {
            return Err("invalid_argument: OCR region is too small".into());
        }
        let cropped = image.crop_imm(x, y, width, height);
        let mut png = std::io::Cursor::new(Vec::new());
        cropped
            .write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        let lines = recognize(png.get_ref())?;
        if lines.len() > 256 || lines.iter().map(|l| l.text.len()).sum::<usize>() > 16000 {
            return Err("visual_text_truncated: refine the region; no verification issued".into());
        }
        let recognized = lines
            .iter()
            .filter(|l| l.confidence >= 0.5)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let matched =
            normalized(&recognized).contains(&normalized(check["text"].as_str().unwrap()));
        observations.push(json!({"text":check["text"],"region":check["region"],"recognizedText":recognized,"matched":matched,"minimumConfidence":0.5}));
    }
    if token.is_cancelled() {
        return Err("cancelled: visual verification cancelled".into());
    }
    let verified = observations.iter().all(|c| c["matched"] == true);
    Ok(
        json!({"status":if verified {"verified_visual_text"} else {"visual_mismatch"},"verified":verified,"engine":"system_ocr","checks":observations,"imageWidth":image.width(),"imageHeight":image.height(),"proofScope":"visible_text_in_requested_window_regions","normalization":"whitespace_only"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_bounds_and_transport_without_guessing_assertions() {
        let good = json!({"text":"测试","region":{"x":0.3,"y":"0.1","width":0.5,"height":0.2}});
        assert!(validate(&json!({"checks":[good.clone()]})).is_ok());
        assert!(validate(&json!({"checks":{"item":good.clone()}})).is_ok());
        for bad in [
            json!([]),
            json!([{"text":" ","region":good["region"]}]),
            json!([{"text":"测试","region":{"x":0.9,"y":0,"width":0.2,"height":0.2}}]),
            json!({"anything":good}),
        ] {
            assert!(validate(&json!({"checks":bad})).is_err());
        }
    }
    #[test]
    fn only_positive_observed_text_in_the_requested_crop_produces_proof() {
        let image = image::DynamicImage::new_rgb8(100, 100);
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let capture =
            json!({"data":base64::engine::general_purpose::STANDARD.encode(png.into_inner())});
        let args = json!({"checks":[{"text":"EchoAgent 微信测试，请忽略。","region":{"x":0.5,"y":0.2,"width":0.4,"height":0.3}}]});
        let token = CancellationToken::new();
        let run = |text: &str, confidence: f32| {
            verify(&capture, &args, &token, |png| {
                let crop = image::load_from_memory(png).unwrap();
                assert_eq!((crop.width(), crop.height()), (40, 30));
                Ok(vec![Line {
                    text: text.into(),
                    confidence,
                }])
            })
            .unwrap()
        };
        assert_eq!(run("EchoAgent 微信测试，\n请忽略。", 1.)["verified"], true);
        assert_eq!(run("EchoAgent 微信测试，请忽略。", 0.49)["verified"], false);
        assert_eq!(run("EchoAgent 微信测试，请忽略!", 1.)["verified"], false);
        assert_eq!(run("完全不同的消息", 1.)["verified"], false);
        token.cancel();
        assert!(verify(&capture, &args, &token, |_| panic!(
            "no OCR after cancellation"
        ))
        .is_err());
    }
}
