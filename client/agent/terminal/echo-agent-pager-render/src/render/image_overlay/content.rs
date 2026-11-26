use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};
use ratatui::text::Span;

use crate::prompt_images::PastedImage;
use crate::render::SafeBuf;
use crate::util::format_bytes;

pub(super) fn paint_path_line(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    path: &Path,
    text_fg: Color,
    bg: Color,
) {
    let raw = path.display().to_string();
    let label = format!(
        "Path: {}",
        truncate_path_for_overlay(&raw, width.saturating_sub(6) as usize)
    );
    let clipped = crate::render::line_utils::truncate_str(&label, width as usize);
    buf.set_span_safe(
        x,
        y,
        &Span::styled(clipped, Style::default().fg(text_fg).bg(bg)),
        width,
    );
}

pub(super) fn build_meta_line(image: &PastedImage, display_path: Option<&Path>) -> String {
    let mut parts = Vec::with_capacity(4);
    parts.push(format_mime(&image.mime_type));
    if let Some((width, height)) = image.preview_dimensions() {
        parts.push(format!("{}x{}", width, height));
    }
    parts.push(format_bytes(image.byte_len as u64));
    if let Some(path) = display_path
        && let Some(name) = path.file_name()
    {
        parts.push(name.to_string_lossy().into_owned());
    }
    parts.join(" \u{00b7} ")
}

pub(super) fn format_mime(mime: &str) -> String {
    match mime {
        "image/png" => "PNG".into(),
        "image/jpeg" => "JPEG".into(),
        "image/tiff" => "TIFF".into(),
        "image/gif" => "GIF".into(),
        "image/webp" => "WebP".into(),
        "image/bmp" => "BMP".into(),
        other => other.into(),
    }
}

pub(super) fn truncate_path_for_overlay(path: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = path.chars().count();
    if char_count <= max_chars {
        return path.to_owned();
    }
    if max_chars <= 3 {
        return path.chars().take(max_chars).collect();
    }
    let keep = max_chars.saturating_sub(3) / 2;
    let end_keep = max_chars.saturating_sub(3) - keep;
    let chars: Vec<char> = path.chars().collect();
    let head: String = chars[..keep].iter().collect();
    let tail: String = chars[chars.len() - end_keep..].iter().collect();
    format!("{head}...{tail}")
}
