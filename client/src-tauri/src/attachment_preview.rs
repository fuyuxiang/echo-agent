//! Safe, bounded thumbnails and on-demand originals for local chat attachments.
//!
//! The frontend deliberately does not expose arbitrary local paths through
//! Tauri's asset protocol. Instead it requests a small JPEG for image formats
//! the agent itself accepts as multimodal prompt content.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use tauri::State;

const MAX_SOURCE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_THUMB_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 20_000;
const MAX_DECODE_ALLOC: u64 = 160 * 1024 * 1024;
const THUMB_LONG_EDGE: u32 = 320;
const THUMB_QUALITY: u8 = 82;

fn supported_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp")
    )
}

fn cache_path(cache_dir: &Path, source: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.to_string_lossy().hash(&mut hasher);
    cache_dir.join(format!("{:016x}.jpg", hasher.finish()))
}

fn cache_is_fresh(cache: &Path, source_modified: Option<SystemTime>) -> bool {
    let Ok(cache_meta) = std::fs::symlink_metadata(cache) else {
        return false;
    };
    if cache_meta.file_type().is_symlink()
        || !cache_meta.is_file()
        || cache_meta.len() == 0
        || cache_meta.len() > MAX_THUMB_BYTES
    {
        return false;
    }
    match (cache_meta.modified().ok(), source_modified) {
        (Some(cache_modified), Some(source_modified)) => cache_modified >= source_modified,
        (Some(_), None) => true,
        _ => false,
    }
}

fn flatten_over_white(image: image::RgbaImage) -> image::RgbImage {
    image::RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let pixel = image.get_pixel(x, y).0;
        let alpha = u16::from(pixel[3]);
        let blend =
            |channel: u8| (((u16::from(channel) * alpha) + (255 * (255 - alpha))) / 255) as u8;
        image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])])
    })
}

fn make_thumbnail_sync(source: &Path, cache_dir: &Path) -> Result<String, String> {
    if !supported_extension(source) {
        return Err("不支持的图片附件格式".into());
    }
    let source_meta =
        std::fs::symlink_metadata(source).map_err(|error| format!("读取图片信息失败：{error}"))?;
    let source_bytes = crate::shell_fs::read_regular_file_bounded(source, MAX_SOURCE_BYTES)
        .map_err(|error| format!("读取图片失败：{error}"))?;

    let cache = cache_path(cache_dir, source);
    if cache_is_fresh(&cache, source_meta.modified().ok()) {
        let bytes = crate::shell_fs::read_regular_file_bounded(&cache, MAX_THUMB_BYTES)
            .map_err(|error| format!("读取缩略图缓存失败：{error}"))?;
        return Ok(base64::engine::general_purpose::STANDARD.encode(bytes));
    }

    let mut reader = image::ImageReader::new(std::io::Cursor::new(source_bytes))
        .with_guessed_format()
        .map_err(|error| format!("识别图片格式失败：{error}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("解码图片失败：{error}"))?;
    let thumbnail = decoded.thumbnail(THUMB_LONG_EDGE, THUMB_LONG_EDGE);
    let rgb = flatten_over_white(thumbnail.to_rgba8());
    let (width, height) = rgb.dimensions();
    let mut jpeg = Vec::with_capacity(32 * 1024);
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, THUMB_QUALITY);
    encoder
        .encode(rgb.as_raw(), width, height, image::ColorType::Rgb8.into())
        .map_err(|error| format!("编码缩略图失败：{error}"))?;

    if let Err(error) = crate::paths::write_private_file(&cache, &jpeg) {
        tracing::warn!(error = %error, path = %cache.display(), "failed to cache attachment thumbnail");
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(jpeg))
}

async fn make_thumbnail(source: PathBuf, cache_dir: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || make_thumbnail_sync(&source, &cache_dir))
        .await
        .map_err(|error| format!("缩略图任务失败：{error}"))?
}

#[tauri::command]
pub async fn attachment_thumbnail(
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    path: String,
) -> Result<String, String> {
    let source = access.require_authorized_file(Path::new(&path))?;
    make_thumbnail(
        source,
        crate::paths::echo_agent_home_dir().join("attachment-thumbs"),
    )
    .await
}

fn preview_data_sync(source: &Path) -> Result<String, String> {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf" | "docx" | "pptx" | "xlsx" | "xls"
    ) {
        return Err("此文件类型请用系统应用打开".into());
    }
    let bytes = crate::shell_fs::read_regular_file_bounded(source, MAX_SOURCE_BYTES)?;
    if supported_extension(source) {
        // Inspect headers before handing an original to the WebView. A tiny
        // compressed source must not expand into an unbounded browser bitmap.
        let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|_| "无法识别图片格式".to_string())?;
        let (width, height) = reader
            .into_dimensions()
            .map_err(|_| "无法读取图片，文件可能已损坏".to_string())?;
        if width == 0
            || height == 0
            || width > MAX_IMAGE_DIMENSION
            || height > MAX_IMAGE_DIMENSION
            || u64::from(width) * u64::from(height) * 4 > MAX_DECODE_ALLOC
        {
            return Err("图片尺寸过大，请用系统应用查看原图".into());
        }
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Full originals are loaded only by an explicit preview gesture. Use the
/// same native file grant, regular-file checks and 20 MiB admission cap as
/// attachments, without broadening the generic binary-reader policy.
#[tauri::command]
pub async fn attachment_preview_data(
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    path: String,
) -> Result<String, String> {
    let source = access.require_authorized_file(Path::new(&path))?;
    tokio::task::spawn_blocking(move || preview_data_sync(&source))
        .await
        .map_err(|error| format!("附件预览任务失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_preview_preserves_image_bytes_above_one_megabyte() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("original.PNG");
        image::RgbaImage::from_pixel(4, 2, image::Rgba([12, 34, 56, 128]))
            .save(&path)
            .unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.resize(1024 * 1024 + 128, 0);
        std::fs::write(&path, &bytes).unwrap();
        let encoded = preview_data_sync(&path).unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            bytes
        );
    }

    #[test]
    fn preview_rejects_oversized_unsupported_and_corrupt_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.pdf");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_SOURCE_BYTES + 1)
            .unwrap();
        assert!(preview_data_sync(&path).is_err());
        let path = temp.path().join("script.exe");
        std::fs::write(&path, b"binary").unwrap();
        assert!(preview_data_sync(&path).unwrap_err().contains("系统应用"));
        let path = temp.path().join("broken.png");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(preview_data_sync(&path).is_err());
    }

    #[test]
    fn document_preview_preserves_original_bytes() {
        let temp = tempfile::tempdir().unwrap();
        for extension in ["pdf", "docx", "pptx", "xlsx", "xls"] {
            let path = temp.path().join(format!("document.{extension}"));
            std::fs::write(&path, b"document bytes").unwrap();
            let encoded = preview_data_sync(&path).unwrap();
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap(),
                b"document bytes"
            );
        }
    }

    #[test]
    fn original_preview_rejects_excessive_image_dimensions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wide.png");
        image::RgbaImage::new(MAX_IMAGE_DIMENSION + 1, 1)
            .save(&path)
            .unwrap();
        assert!(preview_data_sync(&path).unwrap_err().contains("尺寸过大"));
    }

    #[cfg(unix)]
    #[test]
    fn original_preview_rejects_a_source_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("original.pdf");
        let source = temp.path().join("link.pdf");
        std::fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(&target, &source).unwrap();
        assert!(preview_data_sync(&source).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"original");
    }

    #[tokio::test]
    async fn creates_a_bounded_jpeg_and_reuses_the_cache() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("sample.png");
        let cache = temp.path().join("cache");
        let image = image::RgbaImage::from_pixel(640, 320, image::Rgba([12, 34, 56, 128]));
        image.save(&source).expect("write source png");

        let first = make_thumbnail(source.clone(), cache.clone())
            .await
            .expect("create thumbnail");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(first)
            .expect("base64 jpeg");
        assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));
        let cached_files = std::fs::read_dir(&cache)
            .expect("cache directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("cache entries");
        assert_eq!(cached_files.len(), 1);

        let second = make_thumbnail(source, cache)
            .await
            .expect("cached thumbnail");
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(bytes),
            second
        );
    }

    #[tokio::test]
    async fn rejects_non_image_extensions_before_decoding() {
        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("notes.txt");
        std::fs::write(&source, b"not an image").expect("write source");

        let error = make_thumbnail(source, temp.path().join("cache"))
            .await
            .expect_err("text files are not thumbnails");
        assert!(error.contains("不支持"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn poisoned_cache_symlink_is_replaced_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let source = temp.path().join("sample.png");
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(&source)
            .unwrap();
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, "unchanged").unwrap();
        let cache = cache_path(&cache_dir, &source);
        symlink(&outside, &cache).unwrap();

        make_thumbnail(source, cache_dir).await.unwrap();
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "unchanged");
        assert!(!std::fs::symlink_metadata(cache)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
