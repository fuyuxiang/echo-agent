//! Shared prompt-side image types and helpers.
//!
//! This is the single source of truth for image data attached to the prompt.
//! Both the view layer ([`crate::views::prompt_widget`]) and the app/dispatch
//! layer ([`crate::app::dispatch`]) consume these types, so they live here
//! rather than inside any single view or app module.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use echo_agent_ratatui_textarea::ElementId;

/// Tracing target for image-pipeline diagnostics.
///
/// Filter with `RUST_LOG=prompt_images=debug` to surface logs from
/// `insert_image`, `sync_images_with_textarea`, and the drop-classifier
/// (`try_read_dropped_paths`) — useful for RCing cross-platform paste
/// regressions from a single user's session capture.
pub const PROMPT_IMAGES_TRACING_TARGET: &str = "prompt_images";

// -------------------------------------------------------------------------
// Scrollable image viewer state
// -------------------------------------------------------------------------

/// State for a modal image viewer.
///
/// Supports deferred loading: [`open_from_path_deferred`] returns instantly
/// with `loading: true`, then [`finish_loading`] performs the heavy I/O on
/// the next tick so the UI can show a spinner while the file is read.
pub struct ImageViewerState {
    /// Original encoded image bytes.
    pub image_bytes: Vec<u8>,
    /// Encoded bytes prepared for terminal display (e.g. JPEG converted to PNG for Kitty).
    pub display_bytes: Vec<u8>,
    pub mime_type: String,
    pub image_width: u32,
    pub image_height: u32,
    pub display_number: usize,
    /// Override title (e.g. filename). When `None`, uses "Image #N".
    pub title: Option<String>,
    /// Deferred loading in progress; display bytes not yet available.
    pub loading: bool,
    /// Identity used by the shared terminal overlay upload owner.
    pub overlay_owner_id: u64,
    /// File path for deferred loading; consumed by [`finish_loading`].
    source_path: Option<PathBuf>,
    /// Shared modal chrome state (close button hit-test, hover, etc.).
    pub modal_state: crate::modal_window_state::ModalWindowState,
}

pub fn decode_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // Unrestricted: accepts any format `image` recognises so paste previews
    // don't fail for non-allow-listed types.
    echo_agent_tools::util::image_validate::validate_image_bytes_unrestricted(bytes, false)
        .ok()
        .map(|(w, h, _)| (w, h))
}

fn is_decodable_image(bytes: &[u8]) -> bool {
    decode_image_dimensions(bytes).is_some()
}

impl ImageViewerState {
    /// Create a viewer for a prompt-side image. Loads synchronously since
    /// prompt images already have bytes in memory.
    pub fn open(image: &PastedImage) -> Option<Self> {
        let bytes = if let Some(ref b) = image.encoded_bytes {
            b.to_vec()
        } else if let Some(ref path) = image.session_image_path {
            std::fs::read(path).ok()?
        } else {
            return None;
        };

        let (w, h) = decode_image_dimensions(&bytes)?;
        let display_bytes = crate::terminal::image::prepare_overlay_image_bytes(&bytes)
            .unwrap_or_else(|| bytes.clone());

        Some(Self {
            image_bytes: bytes,
            display_bytes,
            mime_type: image.mime_type.clone(),
            image_width: w,
            image_height: h,
            display_number: image.display_number,
            title: None,
            loading: false,
            overlay_owner_id: image.preview.identity(),
            source_path: None,
            modal_state: Default::default(),
        })
    }

    /// Create a viewer from a file path, loading synchronously. Prefer
    /// [`open_from_path_deferred`] from input handlers to avoid blocking.
    pub fn open_from_path(path: &std::path::Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;

        let (w, h) = decode_image_dimensions(&bytes)?;
        let display_bytes = crate::terminal::image::prepare_overlay_image_bytes(&bytes)
            .unwrap_or_else(|| bytes.clone());

        let mime_type = echo_agent_shared::clipboard::mime_from_bytes(&bytes).to_owned();
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());

        Some(Self {
            image_bytes: bytes,
            display_bytes,
            mime_type,
            image_width: w,
            image_height: h,
            display_number: 1,
            title: file_name,
            loading: false,
            overlay_owner_id: crate::terminal::overlay::next_owner_id(),
            source_path: None,
            modal_state: Default::default(),
        })
    }

    /// Create a loading-state viewer for a file path.
    ///
    /// Returns immediately with `loading: true`. A background thread
    /// runs [`load_image_data`] and the tick handler polls for the result,
    /// then calls [`apply_loaded`] to complete the load.
    pub fn open_from_path_deferred(path: &std::path::Path) -> Self {
        Self {
            image_bytes: Vec::new(),
            display_bytes: Vec::new(),
            mime_type: String::new(),
            image_width: 0,
            image_height: 0,
            display_number: 1,
            title: path.file_name().map(|n| n.to_string_lossy().into_owned()),
            loading: true,
            overlay_owner_id: crate::terminal::overlay::next_owner_id(),
            source_path: Some(path.to_path_buf()),
            modal_state: Default::default(),
        }
    }

    /// Take the source path for background loading. Returns `None` if
    /// already taken or not in loading state.
    pub fn take_source_path(&mut self) -> Option<PathBuf> {
        if self.loading {
            self.source_path.take()
        } else {
            None
        }
    }

    /// Apply loaded data from a background thread.
    pub fn apply_loaded(&mut self, data: LoadedImageData) {
        self.image_bytes = data.image_bytes;
        self.display_bytes = data.display_bytes;
        self.mime_type = data.mime_type;
        self.image_width = data.image_width;
        self.image_height = data.image_height;
        self.loading = false;
    }

    /// Complete the deferred load synchronously (convenience for tests).
    /// Returns `false` on failure.
    pub fn finish_loading(&mut self) -> bool {
        if !self.loading {
            return true;
        }
        let Some(path) = self.source_path.take() else {
            return false;
        };
        match load_image_data(&path) {
            ImageLoadResult::Loaded(data) => {
                self.apply_loaded(data);
                true
            }
            ImageLoadResult::Failed => false,
        }
    }
}

/// Result of a background image load.
pub enum ImageLoadResult {
    Loaded(LoadedImageData),
    Failed,
}

/// Data loaded from an image file, ready to apply to the viewer.
pub struct LoadedImageData {
    pub image_bytes: Vec<u8>,
    pub display_bytes: Vec<u8>,
    pub mime_type: String,
    pub image_width: u32,
    pub image_height: u32,
}

/// Load image data from a file path. This is the heavy work (file read,
/// decode, format conversion) that runs on a background thread.
pub fn load_image_data(path: &std::path::Path) -> ImageLoadResult {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("image viewer: failed to read {}: {e}", path.display());
            return ImageLoadResult::Failed;
        }
    };
    let (w, h) = match decode_image_dimensions(&bytes) {
        Some(dims) => dims,
        None => {
            tracing::warn!("image viewer: failed to decode {}", path.display());
            return ImageLoadResult::Failed;
        }
    };
    let display_bytes = crate::terminal::image::prepare_overlay_image_bytes(&bytes)
        .unwrap_or_else(|| bytes.clone());
    let mime_type = echo_agent_shared::clipboard::mime_from_bytes(&bytes).to_owned();

    ImageLoadResult::Loaded(LoadedImageData {
        image_bytes: bytes,
        display_bytes,
        mime_type,
        image_width: w,
        image_height: h,
    })
}

// -------------------------------------------------------------------------
// Video viewer state
// -------------------------------------------------------------------------

/// Target frames per second for terminal video playback.
const VIDEO_FPS: f64 = 10.0;

/// Maximum pixel width for extracted frames.
const VIDEO_MAX_WIDTH: u32 = 640;

/// Modal video viewer state.
///
/// Holds pre-extracted frames and playback position. The rendering path
/// reuses the same `post_flush_escapes` pipeline as the image viewer.
pub struct VideoViewerState {
    /// Pre-extracted frames (PNG for Kitty, JPEG for iTerm2).
    pub frames: Vec<Vec<u8>>,
    /// Current frame index.
    current_frame: usize,
    /// Whether playback is active.
    pub playing: bool,
    /// Playback frame rate.
    pub fps: f64,
    /// Last frame advance timestamp (for pacing).
    last_frame_time: Instant,
    /// Original video pixel dimensions.
    pub video_width: u32,
    pub video_height: u32,
    /// Total duration in seconds.
    pub duration_secs: f64,
    /// Display title (file name).
    pub title: Option<String>,
}

impl VideoViewerState {
    /// Minimal viewer for tests (pager and render unit tests); the real
    /// `open_from_path` needs ffmpeg and a graphics-capable terminal, neither
    /// available under `cargo test`. Public so dependent crates can construct
    /// a viewer without pulling in decode/graphics.
    pub fn test_stub() -> Self {
        Self {
            frames: vec![Vec::new()],
            current_frame: 0,
            playing: false,
            fps: 1.0,
            last_frame_time: Instant::now(),
            video_width: 1,
            video_height: 1,
            duration_secs: 0.0,
            title: None,
        }
    }

    /// Open a video file for playback. Returns `None` if ffmpeg is
    /// unavailable or the video cannot be decoded.
    ///
    /// Extracts all frames upfront — for short videos (5–15s at 10fps)
    /// this is 50–150 frames and takes ~1–3 seconds.
    pub fn open_from_path(path: &std::path::Path) -> Option<Self> {
        use crate::terminal::image::{GraphicsProtocol, detect_graphics_protocol};

        let protocol = detect_graphics_protocol();
        if protocol == GraphicsProtocol::None {
            return None;
        }

        let (width, height, duration, fps) = ffprobe_metadata(path)?;
        let target_fps = VIDEO_FPS.min(fps);

        // PNG for Kitty (required), JPEG for iTerm2 (smaller).
        let ext = match protocol {
            GraphicsProtocol::Kitty => "png",
            GraphicsProtocol::ITerm2 => "jpg",
            GraphicsProtocol::None => return None,
        };

        let vf = if width > VIDEO_MAX_WIDTH {
            format!("fps={target_fps},scale={}:-2", VIDEO_MAX_WIDTH)
        } else {
            format!("fps={target_fps}")
        };

        let tmp_dir = make_temp_dir();
        std::fs::create_dir_all(&tmp_dir).ok()?;

        let mut ffmpeg_cmd = std::process::Command::new("ffmpeg");
        ffmpeg_cmd
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-vf", &vf, "-q:v", "5"])
            .arg(tmp_dir.join(format!("%06d.{ext}")))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut ffmpeg_cmd);
        let status = ffmpeg_cmd.status();

        match &status {
            Err(e) => tracing::debug!("ffmpeg not available: {e}"),
            Ok(s) if !s.success() => tracing::debug!("ffmpeg exited with {s}"),
            _ => {}
        }
        if !status.as_ref().is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return None;
        }

        let frames = load_frames(&tmp_dir, ext);
        let _ = std::fs::remove_dir_all(&tmp_dir);

        if frames.is_empty() {
            return None;
        }

        Some(Self {
            current_frame: 0,
            playing: true,
            fps: target_fps,
            last_frame_time: Instant::now(),
            video_width: width,
            video_height: height,
            duration_secs: duration,
            title: path.file_name().map(|n| n.to_string_lossy().into_owned()),
            frames,
        })
    }

    /// Advance playback based on elapsed time. Returns `true` if the
    /// frame changed (caller should redraw).
    pub fn tick(&mut self) -> bool {
        if !self.playing || self.frames.is_empty() {
            return false;
        }

        let frame_duration = std::time::Duration::from_secs_f64(1.0 / self.fps);
        if self.last_frame_time.elapsed() < frame_duration {
            return false;
        }

        self.current_frame = (self.current_frame + 1) % self.frames.len();
        self.last_frame_time = Instant::now();
        true
    }

    /// Toggle play/pause.
    pub fn toggle_play_pause(&mut self) {
        self.playing = !self.playing;
        if self.playing {
            self.last_frame_time = Instant::now();
        }
    }

    /// Seek forward by ~1 second.
    pub fn seek_forward(&mut self) {
        let skip = self.fps.round() as usize;
        self.current_frame = (self.current_frame + skip).min(self.frames.len().saturating_sub(1));
        self.last_frame_time = Instant::now();
    }

    /// Seek backward by ~1 second.
    pub fn seek_backward(&mut self) {
        let skip = self.fps.round() as usize;
        self.current_frame = self.current_frame.saturating_sub(skip);
        self.last_frame_time = Instant::now();
    }

    /// Current frame image data.
    pub fn current_frame_data(&self) -> &[u8] {
        &self.frames[self.current_frame]
    }

    /// Current playback position in seconds.
    pub fn position_secs(&self) -> f64 {
        if self.fps <= 0.0 {
            return 0.0;
        }
        self.current_frame as f64 / self.fps
    }

    /// Playback progress fraction (0.0–1.0).
    pub fn progress(&self) -> f64 {
        if self.frames.len() <= 1 {
            return 0.0;
        }
        self.current_frame as f64 / (self.frames.len() - 1) as f64
    }
}

/// Extract a single poster frame from a video file via ffmpeg.
/// Returns `(image_bytes, width, height)`. The image format depends on
/// the active terminal protocol (PNG for Kitty, JPEG for iTerm2).
pub fn extract_poster_frame(path: &std::path::Path) -> Option<(Vec<u8>, u32, u32)> {
    use crate::terminal::image::{GraphicsProtocol, detect_graphics_protocol};

    let protocol = detect_graphics_protocol();
    let ext = match protocol {
        GraphicsProtocol::Kitty => "png",
        GraphicsProtocol::ITerm2 => "jpg",
        GraphicsProtocol::None => return None,
    };

    // Try seeking to 1s for a representative frame; fall back to first frame.
    let try_extract = |seek: Option<&str>| {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error"]);
        if let Some(ss) = seek {
            cmd.args(["-ss", ss]);
        }
        cmd.args(["-i"])
            .arg(path)
            .args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", ext])
            .arg("-")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        cmd.output()
            .ok()
            .filter(|o| o.status.success() && !o.stdout.is_empty())
    };

    let output = try_extract(Some("1")).or_else(|| try_extract(None))?;

    let (w, h) = decode_image_dimensions(&output.stdout)?;

    Some((output.stdout, w, h))
}

/// Create a unique temp directory path for frame extraction.
fn make_temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "echo-agent-video-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

/// Read all frame files from `dir` with the given extension, sorted by name.
fn load_frames(dir: &std::path::Path, ext: &str) -> Vec<Vec<u8>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    paths.sort();
    paths.iter().filter_map(|p| std::fs::read(p).ok()).collect()
}

/// Probe video metadata via ffprobe. Returns `(width, height, duration, fps)`.
fn ffprobe_metadata(path: &std::path::Path) -> Option<(u32, u32, f64, f64)> {
    let mut cmd = std::process::Command::new("ffprobe");
    cmd.args([
        "-v",
        "quiet",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,r_frame_rate,duration",
        "-show_entries",
        "format=duration",
        "-of",
        "csv=p=0:s=,",
    ])
    .arg(path)
    .stdin(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("ffprobe not available: {e}");
            return None;
        }
    };

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Stream line: width,height,r_frame_rate[,duration]
    // Format line: duration
    let lines: Vec<&str> = text.trim().lines().collect();
    let parts: Vec<&str> = lines.first()?.split(',').collect();
    if parts.len() < 3 {
        return None;
    }

    let width: u32 = parts[0].trim().parse().ok()?;
    let height: u32 = parts[1].trim().parse().ok()?;
    let fps = parse_fraction(parts[2].trim()).unwrap_or(30.0);

    // Try stream duration, fall back to format duration.
    let duration = parts
        .get(3)
        .and_then(|d| d.trim().parse::<f64>().ok())
        .or_else(|| lines.get(1).and_then(|l| l.trim().parse::<f64>().ok()))
        .unwrap_or(0.0);

    Some((width, height, duration, fps))
}

/// Parse a fraction like "30/1" or "30000/1001".
fn parse_fraction(s: &str) -> Option<f64> {
    if let Some((num, den)) = s.split_once('/') {
        let n: f64 = num.parse().ok()?;
        let d: f64 = den.parse().ok()?;
        if d == 0.0 {
            return None;
        }
        Some(n / d)
    } else {
        s.parse::<f64>().ok()
    }
}

// -------------------------------------------------------------------------
// Inline media info (for scrollback inline rendering)
// -------------------------------------------------------------------------

/// Metadata for inline media rendering in the scrollback.
/// Returned by blocks that want to display media inline.
#[derive(Debug, Clone)]
pub struct InlineMediaInfo {
    /// Path to the media file.
    pub path: PathBuf,
    /// Pixel dimensions of the media.
    pub width: u32,
    pub height: u32,
    /// Whether this is a video (poster frame) vs. a static image.
    pub is_video: bool,
    /// Alt text / description from the markdown `![alt](path)` syntax.
    pub alt_text: String,
}

use echo_agent_shared::clipboard::mime_to_extension;

/// A single image pasted into the prompt.
///
/// Tracks everything needed for display, preview, persistence, and
/// eventual submission as a `ContentBlock::Image`.
#[derive(Debug, Clone)]
pub struct PastedImage {
    /// The [`ElementId`] of the corresponding `KIND_IMAGE` element in the
    /// `TextArea` buffer. Used to reconcile live elements against stored images.
    pub element_id: ElementId,

    /// 1-based display number for the current prompt (e.g. the `1` in
    /// `[Image #1]`). Reset when the prompt is cleared.
    pub display_number: usize,

    /// MIME type of the encoded image (e.g. `"image/png"`).
    pub mime_type: String,

    /// Header-validated dimensions used for pre-insertion/send policy.
    pub dimensions: Option<(u32, u32)>,

    /// Size of the encoded image bytes.
    pub byte_len: usize,

    /// Encoded image bytes kept in memory. Set to `None` once the image
    /// has been durably written to [`session_image_path`](Self::session_image_path)
    /// to avoid holding large buffers for the lifetime of the prompt.
    pub encoded_bytes: Option<Arc<[u8]>>,

    /// Original user-visible path for file-path pastes.
    ///
    /// This remains stable after persistence so previews show the path the
    /// user pasted. Model/send loading uses `session_image_path` first.
    pub source_path: Option<PathBuf>,

    /// Temporary staging path before the image is finalized into the session
    /// directory. Cleaned up when the chip is removed or on send.
    pub staged_temp_path: Option<PathBuf>,

    /// Final durable path under `session_dir(info)/images/`. Once set, this
    /// is the canonical on-disk location and [`encoded_bytes`](Self::encoded_bytes)
    /// may be released.
    pub session_image_path: Option<PathBuf>,

    /// Shared preview preparation state; draw only reads its resolved result.
    pub preview: PromptImagePreview,
}

impl PastedImage {
    pub fn preview_preparation(&self) -> Option<PromptImagePreviewPreparation> {
        if !self.preview.is_pending() {
            return None;
        }
        Some(PromptImagePreviewPreparation {
            preview: self.preview.clone(),
            source: self.encoded_bytes.as_ref()?.clone(),
            protocol: crate::terminal::image::detect_graphics_protocol(),
            dimensions: self.dimensions,
        })
    }

    pub fn prepare_preview_blocking(&self) {
        if let Some(preparation) = self.preview_preparation() {
            preparation.run();
        }
    }

    pub fn preview_dimensions(&self) -> Option<(u32, u32)> {
        self.dimensions.or_else(|| self.preview.dimensions())
    }
}

/// Shared readiness of the terminal pixel payload for a pasted image.
#[derive(Debug, Clone)]
pub struct PromptImagePreview {
    identity: u64,
    result: Arc<OnceLock<PromptImagePreviewResult>>,
}

#[derive(Debug)]
enum PromptImagePreviewResult {
    Ready {
        bytes: Arc<[u8]>,
        dimensions: (u32, u32),
    },
    Failed,
    Unsupported {
        dimensions: (u32, u32),
    },
}

impl Default for PromptImagePreview {
    fn default() -> Self {
        Self {
            identity: crate::terminal::overlay::next_owner_id(),
            result: Arc::new(OnceLock::new()),
        }
    }
}

impl PromptImagePreview {
    pub fn identity(&self) -> u64 {
        self.identity
    }

    pub fn is_pending(&self) -> bool {
        self.result.get().is_none()
    }

    pub fn is_failed(&self) -> bool {
        matches!(self.result.get(), Some(PromptImagePreviewResult::Failed))
    }

    pub fn prepared(&self) -> Option<(&[u8], (u32, u32))> {
        match self.result.get()? {
            PromptImagePreviewResult::Ready { bytes, dimensions } => {
                Some((bytes.as_ref(), *dimensions))
            }
            PromptImagePreviewResult::Failed | PromptImagePreviewResult::Unsupported { .. } => None,
        }
    }

    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match self.result.get()? {
            PromptImagePreviewResult::Ready { dimensions, .. }
            | PromptImagePreviewResult::Unsupported { dimensions } => Some(*dimensions),
            PromptImagePreviewResult::Failed => None,
        }
    }

    fn finish(&self, result: PromptImagePreviewResult) {
        let _ = self.result.set(result);
    }

    pub fn mark_failed(&self) {
        self.finish(PromptImagePreviewResult::Failed);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn ready_for_test(bytes: Vec<u8>, dimensions: (u32, u32)) -> Self {
        let preview = Self::default();
        preview.finish(PromptImagePreviewResult::Ready {
            bytes: Arc::from(bytes),
            dimensions,
        });
        preview
    }
}

/// Off-thread work required to prepare one prompt image preview.
#[derive(Debug, Clone)]
pub struct PromptImagePreviewPreparation {
    preview: PromptImagePreview,
    source: Arc<[u8]>,
    protocol: crate::terminal::image::GraphicsProtocol,
    dimensions: Option<(u32, u32)>,
}

impl PromptImagePreviewPreparation {
    pub fn preview(&self) -> PromptImagePreview {
        self.preview.clone()
    }

    pub fn run(self) {
        let Some(dimensions) = self
            .dimensions
            .or_else(|| decode_image_dimensions(&self.source))
        else {
            self.preview.finish(PromptImagePreviewResult::Failed);
            return;
        };
        let result = match self.protocol {
            crate::terminal::image::GraphicsProtocol::Kitty => {
                let bytes =
                    if crate::terminal::image::kitty_format_from_bytes(&self.source).is_some() {
                        self.source
                    } else {
                        let Some(converted) =
                            crate::terminal::image::prepare_kitty_overlay_image_bytes(&self.source)
                        else {
                            self.preview.finish(PromptImagePreviewResult::Failed);
                            return;
                        };
                        Arc::from(converted)
                    };
                PromptImagePreviewResult::Ready { bytes, dimensions }
            }
            crate::terminal::image::GraphicsProtocol::ITerm2 => PromptImagePreviewResult::Ready {
                bytes: self.source,
                dimensions,
            },
            crate::terminal::image::GraphicsProtocol::None => {
                PromptImagePreviewResult::Unsupported { dimensions }
            }
        };
        self.preview.finish(result);
    }
}

// -------------------------------------------------------------------------
// Display helpers
// -------------------------------------------------------------------------

/// Build the buffer text for an image chip.
///
/// Always path-free: `[Image #1]`. Filepaths live on the [`PastedImage`]
/// record and surface only in the hover/cursor preview overlay — never in
/// the prompt-bar chip.
pub fn display_text(display_number: usize) -> String {
    format!("[Image #{display_number}]")
}

/// Derive a file extension from a MIME type.
///
/// Delegates to [`echo_agent_shared::clipboard::mime_to_extension`].
pub fn extension_for_mime(mime: &str) -> &'static str {
    mime_to_extension(mime)
}

// -------------------------------------------------------------------------
// Reconciliation
// -------------------------------------------------------------------------

/// Remove entries from `images` whose `element_id` is not present in
/// `live_ids`.
///
/// This is the primary mechanism that prevents deleted image chips from
/// being submitted. Call at cleanup boundaries (prompt clear, drain-for-send,
/// explicit chip deletion) rather than on every keystroke.
pub fn reconcile(images: &mut Vec<PastedImage>, live_ids: &HashSet<ElementId>) {
    images.retain(|img| {
        if live_ids.contains(&img.element_id) {
            return true;
        }
        // Clean up temp-file-only staged images for removed chips.
        // Session-persisted files are intentionally left as orphans in v1.
        cleanup_temp_file(img);
        false
    });
}

/// Drain `images`, cleaning up each entry's temp file. The caller
/// is responsible for resetting `image_counter` if appropriate —
/// resetting the counter is a separate semantic concern from clearing
/// a Vec's contents. Use [`reset_counter`] when the counter should
/// also be zeroed.
pub fn drain_and_cleanup(images: &mut Vec<PastedImage>) {
    for img in images.drain(..) {
        cleanup_temp_file(&img);
    }
}

/// Reset the monotonic image counter to 0. Pair with
/// [`drain_and_cleanup`] when a prompt is fully reset (Ctrl+C,
/// `set_text("")`, successful send).
pub fn reset_counter(image_counter: &mut usize) {
    *image_counter = 0;
}

/// Drain images and reset the counter in one call. Prefer
/// [`drain_and_cleanup`] and [`reset_counter`] when only one side
/// is needed; this shim exists so "wipe everything" call sites
/// don't have to repeat the pair.
pub fn clear(images: &mut Vec<PastedImage>, image_counter: &mut usize) {
    drain_and_cleanup(images);
    reset_counter(image_counter);
}

/// Delete a staged temp file if it exists and no session-persisted copy
/// has been made. Session-persisted files are left intact (orphan cleanup
/// is acceptable in v1).
pub fn cleanup_temp_file(img: &PastedImage) {
    if img.session_image_path.is_some() {
        return; // already persisted to session dir, leave it
    }
    if let Some(ref path) = img.staged_temp_path {
        let _ = std::fs::remove_file(path);
    }
}

// -------------------------------------------------------------------------
// Construction from file path
// -------------------------------------------------------------------------

/// Image file extensions recognized when a pasted path is checked.
///
/// Formats omitted on purpose: HEIC/HEIF/AVIF/ICO/SVG. The inline
/// image overlay doesn't decode or render them today, so promoting
/// them to chips would falsely promise rendering. Drops of these
/// extensions fall through to NonImage path text instead.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif"];

/// Normalize a media path by dropping the Windows `\\?\` verbatim prefix
/// (`\\?\C:\x` → `C:\x`, `\\?\UNC\srv\s` → `\\srv\s`). Applied once when a
/// scrollback media ref is built so every consumer (display, open, copy) gets
/// a path that GUI openers and the clipboard can resolve. No-op off Windows.
fn strip_verbatim_prefix(path: &std::path::Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// Drive-letter (`X:\` / `X:/`) or UNC (`\\…`) Windows path. Used to
/// short-circuit [`shell_unescape`], which would otherwise treat the
/// separator backslashes as escape characters and collapse the path.
fn looks_like_windows_path(s: &str) -> bool {
    let b = s.as_bytes();
    let drive = b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/');
    drive || b.starts_with(b"\\\\")
}

/// Strip shell backslash escapes (`\X` → `X`) so terminal-pasted file
/// paths with escaped spaces / parens resolve on disk. Windows-style
/// paths pass through unchanged — see [`looks_like_windows_path`].
fn shell_unescape(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\\') || looks_like_windows_path(s) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => result.push(next),
                None => result.push(c),
            }
        } else {
            result.push(c);
        }
    }
    std::borrow::Cow::Owned(result)
}

/// Strip a single pair of matching ASCII single or double quotes that
/// wrap `s`. Otherwise return `s` unchanged.
fn strip_matching_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Resolve one paste token to a filesystem path.
///
/// Accepts bare paths (with optional shell backslash escapes), `file://`
/// URLs (percent-decoded by the `url` crate), and paths wrapped in a
/// single pair of `"…"` or `'…'` quotes. Returns `None` if a `file://`
/// prefix is present but the URL is not parseable as a local path.
fn token_to_path(token: &str) -> Option<PathBuf> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    let unquoted = strip_matching_quotes(token);

    if unquoted.starts_with("file://") {
        let url = url::Url::parse(unquoted).ok()?;
        if url.scheme() != "file" {
            return None;
        }
        return url.to_file_path().ok();
    }

    let unescaped = shell_unescape(unquoted);
    Some(PathBuf::from(unescaped.into_owned()))
}

/// Validate that `path` points to a readable image file and load it as
/// a [`PastedImage`]. Returns `None` if the extension isn't recognized,
/// the file is missing, empty, or whose bytes don't sniff as an image.
fn read_image_at_path(path: &std::path::Path) -> Option<PastedImage> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    if !path.is_file() {
        return None;
    }

    let data = std::fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }

    let mime_type = echo_agent_shared::clipboard::mime_from_bytes(&data);
    if mime_type == "application/octet-stream" {
        return None;
    }
    let dimensions = decode_image_dimensions(&data)?;

    Some(PastedImage {
        element_id: ElementId::from_raw(0),
        display_number: 0,
        mime_type: mime_type.to_owned(),
        dimensions: Some(dimensions),
        byte_len: data.len(),
        encoded_bytes: Some(Arc::from(data)),
        source_path: Some(path.to_path_buf()),
        staged_temp_path: None,
        session_image_path: None,
        preview: PromptImagePreview::default(),
    })
}

/// Whether `s` begins with a drop-style path anchor: `/`, `~/`, a
/// Windows drive (`X:\` or `X:/`), or a Windows UNC (`\\`). ASCII-only
/// so it never inspects a partial UTF-8 codepoint.
fn starts_with_path_anchor(s: &str) -> bool {
    let b = s.as_bytes();
    matches!(b.first(), Some(b'/'))
        || b.starts_with(b"~/")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
        || b.starts_with(b"\\\\")
}

/// Whether `s` begins with something the space-splitter should treat as
/// a path token boundary: a bare path anchor, a `file://` URL, or a
/// quoted form of either (quotes are stripped before re-checking).
pub fn starts_with_drop_anchor(s: &str) -> bool {
    if starts_with_path_anchor(s) || s.starts_with("file://") {
        return true;
    }
    let unq = strip_matching_quotes(s);
    !std::ptr::eq(unq, s) && (starts_with_path_anchor(unq) || unq.starts_with("file://"))
}

/// Split `s` on each space that is immediately followed by a drop-style
/// anchor: a bare path start (`/`, `~/`, `X:\`) or a `file://` URL.
/// Operates on ASCII bytes (the only characters we need to match) and
/// never splits inside a multi-byte UTF-8 sequence because all match
/// bytes are ASCII.
fn split_space_before_path(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' && starts_with_drop_anchor(&s[i + 1..]) {
            parts.push(&s[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

/// Tokenize one trimmed line into one or more path candidates.
///
/// Returns the whole line as a single token unless every space-separated
/// part itself starts with a drop-style anchor — the all-parts gate
/// keeps prose like `"check /tmp/foo.png please"` or bash pastes like
/// `"! /tmp/foo.png"` from being mis-split. Empty input yields an empty
/// `Vec` so the caller's `flat_map` skips blank lines cleanly.
fn space_split_line(line: &str) -> Vec<&str> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let parts: Vec<&str> = split_space_before_path(line)
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.len() > 1 && parts.iter().all(|p| starts_with_drop_anchor(p)) {
        parts
    } else {
        vec![line]
    }
}

/// Normalize line endings (`\r\n`/`\r` → `\n`) without allocating when
/// the input has no carriage returns (the common case from macOS).
fn normalize_line_endings(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Filter the output of [`try_read_dropped_paths`] down to image
/// entries only — kept as a stable API for prior image-only callers.
/// See [`try_read_dropped_paths`] for the full tokenisation and
/// decoding behaviour spec.
pub fn try_read_images_from_paste(text: &str) -> Vec<PastedImage> {
    try_read_dropped_paths(text)
        .into_iter()
        .filter_map(|d| match d {
            DroppedPath::Image(img) => Some(img),
            DroppedPath::NonImage(_) => None,
        })
        .collect()
}

/// Classification of a single drop-style paste token.
///
/// Returned by [`try_read_dropped_paths`]. Images are routed to
/// `[Image #N]` chip insertion; non-images are inserted as decoded
/// absolute path text so the user can reference them by path / let the
/// agent read them.
#[derive(Debug)]
pub enum DroppedPath {
    /// Token resolved to a readable image file (extension in
    /// [`IMAGE_EXTENSIONS`] and bytes sniff as a known image format).
    Image(PastedImage),
    /// Token resolved to a `file://` URL or an existing on-disk path
    /// (file *or* directory) that is not a recognised image — the
    /// caller should insert this decoded path as plain text in the
    /// prompt.
    ///
    /// The stored `PathBuf` is canonicalised when possible
    /// (`canonicalize()` succeeds) so symlinks resolve to their target,
    /// matching the image branch's `read_image_at_path` behaviour. The
    /// raw decoded path is used as a fallback when canonicalisation
    /// fails (broken symlinks, permission errors, network mounts that
    /// are unreachable, or `file://` URLs to non-existent paths).
    #[allow(dead_code)]
    NonImage(PathBuf),
}

/// Resolve one paste token to either an image chip, a non-image path
/// for text insertion, or `None` when the token does not look like a
/// drop-event path at all.
///
/// Non-image bare paths must satisfy *three* conditions to be
/// intercepted: the token must start with a drop anchor (`/`, `~/`,
/// `X:\`), the path must exist on disk (file OR directory), and the
/// raw bytes must not decode as an image (else the image branch wins).
/// The drop-anchor gate prevents arbitrary prose strings that happen
/// to coincide with a filesystem path from being intercepted from
/// inside a sentence. `file://` URLs bypass both the anchor gate and
/// the existence gate (an explicit URI is unambiguous drop intent).
///
/// Predicate order: cheap anchor/`file://` checks run first; the
/// (relatively) more expensive [`read_image_at_path`] file-read +
/// magic-byte sniff runs only for tokens that pass the gate. Bare
/// cwd-relative image filenames are intentionally NOT intercepted —
/// drag-and-drop / Finder-paste always emit absolute paths or
/// `file://` URLs, never `foo.png`-style relative refs.
///
/// **Silent fallthroughs**: a typo'd `file://` URL (missing path)
/// and an image-extension-but-garbage-bytes drop both land as text
/// with no toast. Drops can be partial (network mounts, broken
/// symlinks, corrupted exports) and a toast for every such case
/// would be noisier than the silent path-as-text behaviour the
/// user already gets.
fn try_read_dropped_path(token: &str) -> Option<DroppedPath> {
    let trimmed_unq = strip_matching_quotes(token.trim());
    let is_file_url = trimmed_unq.starts_with("file://");
    let is_bare_anchored = starts_with_path_anchor(trimmed_unq);
    if !is_file_url && !is_bare_anchored {
        return None;
    }
    let path = token_to_path(token)?;
    // Reject empty or root paths. `file:///` decodes to an empty
    // path on some platforms or to `/` on others; either way it is
    // never a legitimate drop target (root would force the user to
    // "attach the entire filesystem").
    if path.as_os_str().is_empty() || path == std::path::Path::new("/") {
        return None;
    }
    // Reject the bytes that actually corrupt the terminal/text-paste
    // pipeline — NUL, CR, LF — produced by pathological encodings
    // like `file:///path%00.png`. TAB and other low-control bytes
    // are legal in Unix filenames and tolerated by the TUI text
    // rendering, so they pass through.
    if path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|&b| b == 0 || b == b'\r' || b == b'\n')
    {
        return None;
    }
    // Image branch wins when extension + magic bytes both match —
    // keeps the established "`[Image #N]` for image drops" behaviour
    // for tokens that already passed the anchor / `file://` gate.
    if let Some(img) = read_image_at_path(&path) {
        return Some(DroppedPath::Image(img));
    }
    // `file://` is an unambiguous drop URI even when the file is
    // missing (stale links, network mounts). Bare anchored paths
    // require the target to *exist* (file or directory) so that prose
    // typed at the path-anchor start, e.g. `/tmp is a dir on Unix`,
    // falls through to plain text paste.
    if !is_file_url && !path.exists() {
        return None;
    }
    // Canonicalise when possible so the inserted text matches what
    // other code paths see (e.g. cwd-relative comparisons). Fall back
    // to the raw decoded path when canonicalisation fails (broken
    // symlinks, permission issues, network mounts, `file://` URLs to
    // missing files).
    let resolved = dunce::canonicalize(&path).unwrap_or(path);
    Some(DroppedPath::NonImage(resolved))
}

/// Parse `text` from a terminal paste into a list of [`DroppedPath`]
/// entries — images and non-image file paths interleaved in the order
/// they appeared.
///
/// This is the superset routine used by the drag-and-drop / Finder-paste
/// pipeline. It handles `file://` URLs (percent-decoded, including
/// `%20`/`%23`/`%3F` etc.), bare absolute paths, shell-escaped tokens,
/// quoted tokens, and multi-file payloads (newline- or space-separated).
/// Trailing whitespace and CRLF/CR line endings are tolerated.
///
/// Non-image bare paths are only intercepted when the token itself
/// begins with a drop anchor (`/`, `~/`, `X:\`) **and** the path
/// exists on disk. This guards against prose that happens to coincide
/// with a filesystem path being eaten from inside a sentence. See
/// [`try_read_dropped_path`] for the full predicate.
///
/// **Whole-paste-or-nothing.** A paste of
/// `"file:///foo.png\nplease look at this"` must not emit just the
/// image and silently lose the comment — "screenshot URL + caption"
/// pastes are common in practice (browser/Slack right-click
/// "Copy image address" plus hand-typed prose). The function
/// returns an empty `Vec` if *any* non-whitespace line fails to
/// resolve to a drop, so the caller falls through to plain-text
/// paste of the whole payload.
///
/// Concretely:
/// - Empty/whitespace lines are separators (skipped).
/// - For each non-empty line, all tokens emitted by
///   [`space_split_line`] must resolve. If any fail, the whole paste
///   falls through to prose.
/// - If every non-empty line fully resolves, entries are emitted in
///   source order.
///
/// Returns an empty `Vec` when no token resolves to either an image
/// or a recognised file path — callers should then treat the payload
/// as a plain text paste.
pub fn try_read_dropped_paths(text: &str) -> Vec<DroppedPath> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let normalized = normalize_line_endings(trimmed);
    let mut result = Vec::new();
    for line in normalized.split('\n') {
        let tokens = space_split_line(line);
        if tokens.is_empty() {
            // Blank line — treat as separator.
            continue;
        }
        let resolved: Vec<DroppedPath> = tokens
            .iter()
            .filter_map(|t| try_read_dropped_path(t))
            .collect();
        // Any-line-fails-falls-through: even a single-token line that
        // doesn't resolve poisons the whole paste so the user's prose
        // is preserved via the caller's plain-text-paste fallback.
        if resolved.len() < tokens.len() {
            return Vec::new();
        }
        result.extend(resolved);
    }
    let (images, non_images) = result.iter().fold((0usize, 0usize), |(i, n), e| match e {
        DroppedPath::Image(_) => (i + 1, n),
        DroppedPath::NonImage(_) => (i, n + 1),
    });
    tracing::debug!(
        target: PROMPT_IMAGES_TRACING_TARGET,
        input_len = text.len(),
        anchor = paste_anchor_kind(trimmed),
        images,
        non_images,
        "try_read_dropped_paths verdict",
    );
    result
}

/// Categorical label for the leading bytes of a paste, used in
/// diagnostic logging only. Lossy by design — the classifier itself
/// re-derives the real predicate.
fn paste_anchor_kind(trimmed: &str) -> &'static str {
    if trimmed.starts_with("file://") {
        "file_url"
    } else if trimmed.starts_with('/') {
        "absolute"
    } else if trimmed.starts_with("~/") {
        "tilde"
    } else if trimmed.len() >= 3
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
        && &trimmed.as_bytes()[1..3] == b":\\"
    {
        "windows_drive"
    } else if trimmed.starts_with("\\\\") {
        "windows_unc"
    } else {
        "none"
    }
}

/// Check whether `text` looks like a single file path to an image that
/// exists on disk. Returns the loaded image if so.
///
/// Thin wrapper over [`try_read_images_from_paste`] that returns `Some`
/// only when the paste resolves to exactly one image; returns `None`
/// for zero or multiple images. Trailing whitespace (including a single
/// trailing `\n` or `\r\n`) is tolerated; multi-image payloads return
/// `None` so the caller can route them through the multi-image helper
/// instead.
pub fn try_read_image_from_path(text: &str) -> Option<PastedImage> {
    let mut images = try_read_images_from_paste(text);
    if images.len() == 1 {
        Some(images.remove(0))
    } else {
        None
    }
}

// -------------------------------------------------------------------------
// Construction from clipboard data
// -------------------------------------------------------------------------

/// Build a `PastedImage` from raw clipboard [`ImageData`].
///
/// `element_id` and `display_number` are set to placeholder values and
/// will be overwritten by [`crate::views::prompt_widget::PromptWidget::insert_image`].
pub fn from_clipboard_data(data: &crate::clipboard::ImageData) -> PastedImage {
    PastedImage {
        element_id: ElementId::from_raw(0),
        display_number: 0,
        mime_type: data.mime_type.clone(),
        dimensions: decode_image_dimensions(&data.data),
        byte_len: data.data.len(),
        encoded_bytes: Some(Arc::from(data.data.clone())),
        source_path: None,
        staged_temp_path: None,
        session_image_path: None,
        preview: PromptImagePreview::default(),
    }
}

// -------------------------------------------------------------------------
// Session image persistence
// -------------------------------------------------------------------------

/// Persist image bytes into the session `images/` directory.
///
/// Creates the directory if it does not exist. Uses a UUID-v4 filename
/// to avoid collisions (two pastes of the same image are independent).
///
/// On success:
/// - Sets `img.session_image_path` to the written file.
/// - Leaves `img.source_path` unchanged as the original display path.
/// - Drops `encoded_bytes` from memory.
pub fn persist_to_session(
    img: &mut PastedImage,
    session_images_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let bytes = img
        .encoded_bytes
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no encoded bytes to persist"))?;

    std::fs::create_dir_all(session_images_dir)?;

    let ext = extension_for_mime(&img.mime_type);
    let filename = format!("image-{}.{}", uuid::Uuid::new_v4(), ext);
    let path = session_images_dir.join(&filename);
    // Atomic write: a crash mid-write leaves no partially-written final file.
    let tmp_path = path.with_extension(format!("{ext}.tmp"));
    let write_result: anyhow::Result<()> = (|| {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    write_result?;

    img.session_image_path = Some(path);
    img.encoded_bytes = None;
    Ok(())
}

/// Derive the `images/` directory for a session.
///
/// Returns `None` if session identity is not yet known.
pub fn session_images_dir(
    session_id: Option<&agent_client_protocol::SessionId>,
    cwd: &std::path::Path,
) -> Option<PathBuf> {
    let sid = session_id?;
    let info = echo_agent_shared::session::info::Info {
        id: sid.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
    };
    Some(echo_agent_shared::session::session_dir(&info).join("images"))
}

/// Derive the `mermaid/` cache directory for a session.
///
/// Mirrors [`session_images_dir`]: rendered diagram PNGs live alongside the
/// session's other artifacts (`events.jsonl`, `images/`) so they are owned by
/// the session and torn down with it. Returns `None` until session identity is
/// known (no diagrams are cached on disk before then).
pub fn session_mermaid_dir(
    session_id: Option<&agent_client_protocol::SessionId>,
    cwd: &std::path::Path,
) -> Option<PathBuf> {
    let sid = session_id?;
    let info = echo_agent_shared::session::info::Info {
        id: sid.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
    };
    Some(echo_agent_shared::session::session_dir(&info).join("mermaid"))
}

// -------------------------------------------------------------------------
// Image loading for send
// -------------------------------------------------------------------------

const MAX_SEND_BYTES: usize = 50_000_000; // 50 MB

/// Load image bytes from a `PastedImage` (in-memory or from disk).
/// Returns `None` if the image cannot be loaded or exceeds [`MAX_SEND_BYTES`].
pub fn load_for_send(img: &PastedImage) -> Option<(Vec<u8>, String)> {
    let raw_bytes = if let Some(ref b) = img.encoded_bytes {
        b.to_vec()
    } else if let Some(ref path) = img.session_image_path {
        match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "image {}: failed to read {}: {e}",
                    img.display_number,
                    path.display()
                );
                return None;
            }
        }
    } else {
        tracing::warn!("image {}: no bytes or file path", img.display_number);
        return None;
    };

    if raw_bytes.len() > MAX_SEND_BYTES {
        tracing::warn!(
            "image {}: {} bytes exceeds send limit, skipping",
            img.display_number,
            raw_bytes.len()
        );
        return None;
    }
    if let Some((width, height)) = img.dimensions
        && (width < 8 || height < 8)
    {
        tracing::warn!(
            "image {}: dimensions {}x{} are below the 8x8 minimum",
            img.display_number,
            width,
            height
        );
        return None;
    }

    Some((raw_bytes, img.mime_type.clone()))
}

// -------------------------------------------------------------------------
// ACP content block construction
// -------------------------------------------------------------------------

/// Build ACP `ContentBlock` values from prompt text and attached images,
/// with an optional fallback that re-loads orphan
/// `[Image #N: <path>]` placeholders from disk.
///
/// Behaviour for each placeholder in `text`:
/// - If a [`PastedImage`] with the matching `display_number` is present
///   in `images`, the placeholder is left untouched and the
///   `PastedImage` provides the bytes.
/// - Otherwise, if `workspace_cwd` is `Some`, attempt to load the
///   placeholder's path via the shared
///   [`echo_agent_runtime::session::placeholder_images::load_placeholder_image`]
///   helper. On success: attach a `ContentBlock::Image` and leave the
///   placeholder text in place. On failure: strip the placeholder from
///   the forwarded text and emit a `tracing::warn!` (no UI alert
///   surface exists today — the warn log is the established pattern,
///   see `load_for_send` for prior art).
/// - When `workspace_cwd` is `None`, the orphan placeholder is left in
///   the text unchanged (legacy behaviour, used by unit tests).
///
/// Path validation, extension allowlist, and the 50-MB size cap come
/// from the shared helper so the TUI and the server use the same rules.
pub fn build_content_blocks_with_workspace(
    text: String,
    images: Vec<PastedImage>,
    workspace_cwd: Option<&std::path::Path>,
) -> Vec<agent_client_protocol::ContentBlock> {
    let allowed: Option<Vec<std::path::PathBuf>> =
        workspace_cwd.map(echo_agent_shared::placeholder_images::default_allowed_prefixes);
    build_content_blocks_with_prefixes(text, images, allowed.as_deref())
}

/// Test-injectable variant of [`build_content_blocks_with_workspace`].
///
/// Accepts an explicit `allowed_prefixes` slice so unit tests can
/// pass a hermetic prefix list and avoid reading the ambient process
/// `$HOME`. Production calls go through
/// [`build_content_blocks_with_workspace`].
pub fn build_content_blocks_with_prefixes(
    text: String,
    images: Vec<PastedImage>,
    allowed_prefixes: Option<&[std::path::PathBuf]>,
) -> Vec<agent_client_protocol::ContentBlock> {
    build_content_blocks_with_prefixes_and_caps(
        text,
        images,
        allowed_prefixes,
        echo_agent_shared::placeholder_images::MAX_PLACEHOLDER_AGGREGATE_BYTES,
    )
}

/// Test-injectable variant of [`build_content_blocks_with_prefixes`]
/// that takes an explicit aggregate-bytes cap.
///
/// Mirrors the server-side
/// [`echo_agent_runtime::session::placeholder_images::recover_orphan_placeholders_with_prefixes_and_caps`].
/// Aggregate-cap semantics match: `aggregate + image.len() > cap`
/// triggers the loop break (inclusive boundary — a running total
/// exactly equal to the cap is admitted).
pub fn build_content_blocks_with_prefixes_and_caps(
    text: String,
    images: Vec<PastedImage>,
    allowed_prefixes: Option<&[std::path::PathBuf]>,
    aggregate_max: usize,
) -> Vec<agent_client_protocol::ContentBlock> {
    use agent_client_protocol::{ContentBlock, ImageContent, TextContent};
    use base64::Engine as _;

    // Phase 1: rewrite the text to strip failed-load placeholders, and
    // collect successfully-loaded orphan images. PastedImage-backed
    // placeholders (display_number present in `images`) are left alone.
    let (rewritten_text, orphan_images) =
        resolve_orphan_placeholders(text, &images, allowed_prefixes, aggregate_max);

    // Phase 2: drop `[Image #N: <path>]` → `[Image #N]`. The path
    // tempts the model into a redundant `Read` on its own
    // attachment.
    let rewritten_text =
        echo_agent_shared::placeholder_images::strip_paths_from_image_placeholders(rewritten_text);

    let mut blocks = Vec::with_capacity(1 + images.len() + orphan_images.len());
    blocks.push(ContentBlock::Text(TextContent::new(rewritten_text)));

    for img in &images {
        let (bytes, mime_type) = match load_for_send(img) {
            Some(loaded) => loaded,
            None => continue,
        };

        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

        // Canonicalize the display path only when no durable wire path exists.
        let uri = if let Some(path) = img.session_image_path.as_ref() {
            Some(format!("file://{}", path.display()))
        } else {
            img.source_path
                .as_ref()
                .and_then(|path| dunce::canonicalize(path).ok())
                .map(|canonical| format!("file://{}", canonical.display()))
        };

        blocks.push(ContentBlock::Image(
            ImageContent::new(data, mime_type)
                .uri(uri)
                // Record the `[Image #N]` display number so the server resolves
                // the token by number, not list position. See `AttachedImages`.
                .meta(Some(
                    echo_agent_shared::placeholder_images::display_number_meta(img.display_number),
                )),
        ));
    }

    for orphan in orphan_images {
        blocks.push(ContentBlock::Image(orphan));
    }

    blocks
}

/// Scan `text` for `[Image #N: <path>]` placeholders that lack a
/// matching [`PastedImage`] and attempt to recover them from disk.
///
/// Returns `(rewritten_text, recovered_images)`:
/// - Placeholders with a matching `PastedImage` (by `display_number`)
///   are left untouched.
/// - Orphan placeholders whose path loads successfully via the shared
///   helper are kept in the text and produce a recovered
///   `ImageContent`.
/// - Orphan placeholders whose path fails to load are stripped from
///   the text and a `tracing::warn!` is emitted.
///
/// `allowed_prefixes == None` short-circuits to the legacy behaviour:
/// the text is returned unchanged and no recovery is attempted.
fn resolve_orphan_placeholders(
    text: String,
    images: &[PastedImage],
    allowed_prefixes: Option<&[std::path::PathBuf]>,
    aggregate_max: usize,
) -> (String, Vec<agent_client_protocol::ImageContent>) {
    use agent_client_protocol::ImageContent;
    use base64::Engine as _;

    let Some(allowed) = allowed_prefixes else {
        return (text, Vec::new());
    };

    let attached_numbers: std::collections::HashSet<usize> =
        images.iter().map(|i| i.display_number).collect();

    let placeholders = echo_agent_shared::placeholder_images::extract_placeholders(&text);
    if placeholders.is_empty() {
        return (text, Vec::new());
    }

    let mut recovered: Vec<ImageContent> = Vec::new();
    // Spans to delete from the text (failed loads). Recorded as
    // half-open byte ranges so we can splice them out in one pass.
    let mut strip_spans: Vec<(usize, usize)> = Vec::new();
    let mut aggregate_bytes: usize = 0;

    for ph in &placeholders {
        if attached_numbers.contains(&ph.display_number) {
            continue; // PastedImage already supplies these bytes.
        }
        match echo_agent_shared::placeholder_images::load_placeholder_image(&ph.path, allowed) {
            Ok(loaded) => {
                let next_total = aggregate_bytes.saturating_add(loaded.data.len());
                if next_total > aggregate_max {
                    tracing::warn!(
                        path = ?ph.path,
                        aggregate_bytes,
                        per_image_bytes = loaded.data.len(),
                        cap = aggregate_max,
                        "TUI placeholder fallback: aggregate-bytes cap reached; skipping remaining orphan placeholders",
                    );
                    break;
                }
                aggregate_bytes = next_total;
                let data = base64::engine::general_purpose::STANDARD.encode(&loaded.data);
                let uri = dunce::canonicalize(std::path::Path::new(&ph.path))
                    .ok()
                    .map(|p| format!("file://{}", p.display()));
                recovered.push(
                    ImageContent::new(data, loaded.mime_type)
                        .uri(uri)
                        // Same `[Image #N]` → number mapping as inline images.
                        .meta(Some(
                            echo_agent_shared::placeholder_images::display_number_meta(
                                ph.display_number,
                            ),
                        )),
                );
                tracing::info!(
                    path = ?ph.path,
                    "TUI placeholder fallback: loaded orphan image from disk",
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = ?ph.path,
                    error = %e,
                    "TUI placeholder fallback: orphan image failed to load; stripping placeholder text",
                );
                strip_spans.push(ph.span);
            }
        }
    }

    if strip_spans.is_empty() {
        return (text, recovered);
    }

    // Splice out the failed-load spans in reverse order so earlier
    // indices stay valid. Only collapse the single whitespace seam
    // created by the strip itself — global collapsing of all
    // 2+-space runs would mangle code blocks, indentation-sensitive
    // markdown, and double-space punctuation elsewhere in the text.
    let mut rewritten = text;
    strip_spans.sort_by_key(|(s, _)| *s);
    for (start, end) in strip_spans.into_iter().rev() {
        collapse_strip_seam(&mut rewritten, start, end);
    }
    (rewritten, recovered)
}

/// Splice out `[start, end)` from `text` and, only if both sides of
/// the seam are ASCII whitespace, collapse the run to a single space.
/// Newlines are preserved (treated as non-collapsible boundaries).
fn collapse_strip_seam(text: &mut String, start: usize, end: usize) {
    text.replace_range(start..end, "");
    // After removal `start` is the seam position. Walk left/right
    // over the immediate space chars only; do not cross newlines or
    // non-space whitespace (tab/CR).
    let bytes = text.as_bytes();
    let mut left = start;
    while left > 0 && bytes.get(left - 1) == Some(&b' ') {
        left -= 1;
    }
    let mut right = start;
    while right < bytes.len() && bytes.get(right) == Some(&b' ') {
        right += 1;
    }
    if right - left > 1 {
        // Replace the run with a single space if there is content on
        // both sides; otherwise (seam at start/end of text) trim
        // entirely.
        let has_left_content = left > 0;
        let has_right_content = right < bytes.len();
        let replacement = if has_left_content && has_right_content {
            " "
        } else {
            ""
        };
        text.replace_range(left..right, replacement);
    }
}

// -------------------------------------------------------------------------
// Scrollback image references
// -------------------------------------------------------------------------

/// An image file referenced in scrollback content via `![alt](path)` markdown
/// or a bare absolute path. Validated on construction: path must exist, have a
/// recognized image extension, and decode successfully.
#[derive(Debug, Clone)]
pub struct ScrollbackImageRef {
    /// Absolute path to the image file on disk.
    pub path: PathBuf,
    /// Pixel dimensions `(width, height)`, decoded on construction.
    pub dimensions: Option<(u32, u32)>,
    /// Alt text from the `![alt](path)` markdown syntax (empty for bare paths).
    pub alt_text: String,
}

impl ScrollbackImageRef {
    /// Construct from a file path, returning `None` if the path doesn't
    /// exist, isn't a file, lacks a recognized image extension, or can't
    /// be decoded as an image.
    pub fn from_path(path: impl Into<PathBuf>) -> Option<Self> {
        Self::from_path_with_alt(path, String::new())
    }

    /// Construct with alt text from the markdown `![alt](path)` syntax.
    pub fn from_path_with_alt(path: impl Into<PathBuf>, alt_text: String) -> Option<Self> {
        let path = strip_verbatim_prefix(&path.into());
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            return None;
        }
        if !path.is_file() {
            return None;
        }
        let bytes = std::fs::read(&path).ok()?;
        if !is_decodable_image(&bytes) {
            return None;
        }
        let dimensions = decode_image_dimensions(&bytes);
        Some(Self {
            path,
            dimensions,
            alt_text,
        })
    }
}

/// Regex pattern for `![alt](path)` — captures alt text (group 1) and path (group 2).
const MARKDOWN_IMAGE_REF_PATTERN: &str = r"!\[([^\]]*)\]\(([^)\s]+)\)";

/// Whether text consists only of markdown media references (`![alt](path)`).
///
/// `resolved_ref_count` is the total number of resolved media refs
/// (images + videos) extracted from the same text. Unresolved or
/// undecodable paths are not counted, preventing false positives.
pub fn is_media_only_markdown(text: &str, resolved_ref_count: usize) -> bool {
    use std::sync::LazyLock;

    if resolved_ref_count == 0 {
        return false;
    }

    static ONLY_MD_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(&format!(r"^\s*(?:{}\s*)+$", MARKDOWN_IMAGE_REF_PATTERN)).unwrap()
    });
    static MD_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(MARKDOWN_IMAGE_REF_PATTERN).unwrap());

    if !ONLY_MD_RE.is_match(text) {
        return false;
    }

    let unique_ref_count = MD_RE
        .captures_iter(text)
        .filter_map(|cap| cap.get(2).map(|m| m.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .len();
    unique_ref_count == resolved_ref_count
}

/// Extract image references from text (markdown or tool output).
///
/// Scans for `![alt](path)` patterns and bare absolute image paths.
/// Only returns references where the file exists on disk and decodes as an
/// image.
pub fn extract_image_refs(text: &str) -> Vec<ScrollbackImageRef> {
    use std::sync::LazyLock;

    static MD_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(MARKDOWN_IMAGE_REF_PATTERN).unwrap());

    static PATH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        let exts = IMAGE_EXTENSIONS.join("|");
        // Unix absolute paths (/...) and Windows absolute paths (C:\..., C:/...,
        // or \\... UNC). Drive letters accept either separator.
        regex::Regex::new(&format!(
            r"(?:^|[\s,])((?:/|[A-Za-z]:[\\/]|\\\\)[^\s,]+\.(?:{exts}))(?:[\s,.(]|$)"
        ))
        .unwrap()
    });

    let mut refs = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in MD_RE.captures_iter(text) {
        if let Some(m) = cap.get(2) {
            let path_str = m.as_str();
            let alt_text = cap
                .get(1)
                .map(|a| a.as_str().to_owned())
                .unwrap_or_default();
            if seen.insert(path_str.to_owned())
                && let Some(r) = ScrollbackImageRef::from_path_with_alt(path_str, alt_text)
            {
                refs.push(r);
            }
        }
    }

    for cap in PATH_RE.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let path_str = m.as_str();
            if seen.insert(path_str.to_owned())
                && let Some(r) = ScrollbackImageRef::from_path(path_str)
            {
                refs.push(r);
            }
        }
    }

    refs
}

// -------------------------------------------------------------------------
// Scrollback video references
// -------------------------------------------------------------------------

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mov", "avi", "mkv"];

/// A video file referenced in scrollback content via `![alt](path.mp4)` markdown
/// or a bare absolute path. Validated on construction: path must exist and have a
/// recognized video extension.
#[derive(Debug, Clone)]
pub struct ScrollbackVideoRef {
    /// Absolute path to the video file on disk.
    pub path: PathBuf,
    /// Alt text from the `![alt](path)` markdown syntax (empty for bare paths).
    pub alt_text: String,
}

impl ScrollbackVideoRef {
    /// Validate that `path` exists and has a recognized video extension.
    pub fn from_path(path: impl Into<PathBuf>) -> Option<Self> {
        Self::from_path_with_alt(path, String::new())
    }

    /// Construct with alt text from the markdown `![alt](path)` syntax.
    pub fn from_path_with_alt(path: impl Into<PathBuf>, alt_text: String) -> Option<Self> {
        let path = strip_verbatim_prefix(&path.into());
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !VIDEO_EXTENSIONS.contains(&ext.as_str()) || !path.is_file() {
            return None;
        }
        Some(Self { path, alt_text })
    }
}

/// Extract video references from text (markdown `![](path)` and bare paths).
pub fn extract_video_refs(text: &str) -> Vec<ScrollbackVideoRef> {
    use std::sync::LazyLock;

    // Reuse the markdown image ref pattern — video_gen uses ![prompt](path.mp4).
    static MD_RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(MARKDOWN_IMAGE_REF_PATTERN).unwrap());

    static VIDEO_PATH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        let exts = VIDEO_EXTENSIONS.join("|");
        // Unix absolute paths (/...) and Windows absolute paths (C:\..., C:/...,
        // or \\... UNC). Drive letters accept either separator.
        regex::Regex::new(&format!(
            r"(?:^|[\s,])((?:/|[A-Za-z]:[\\/]|\\\\)[^\s,]+\.(?:{exts}))(?:[\s,.(]|$)"
        ))
        .unwrap()
    });

    let mut refs = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // MD_RE: group 1 = alt text, group 2 = path
    for cap in MD_RE.captures_iter(text) {
        if let Some(m) = cap.get(2) {
            let path_str = m.as_str();
            let alt_text = cap
                .get(1)
                .map(|a| a.as_str().to_owned())
                .unwrap_or_default();
            if seen.insert(path_str.to_owned())
                && let Some(r) = ScrollbackVideoRef::from_path_with_alt(path_str, alt_text)
            {
                refs.push(r);
            }
        }
    }

    // VIDEO_PATH_RE: group 1 = path (no alt text)
    for cap in VIDEO_PATH_RE.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let path_str = m.as_str();
            if seen.insert(path_str.to_owned())
                && let Some(r) = ScrollbackVideoRef::from_path(path_str)
            {
                refs.push(r);
            }
        }
    }

    refs
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The image dir is keyed off the session's cwd — the glue the cross-cwd
    /// resume fix relies on: with `AgentSession.cwd` anchored to the origin cwd,
    /// pasted images land under that origin, not the process cwd.
    #[test]
    fn session_images_dir_keys_off_the_passed_cwd() {
        let id = agent_client_protocol::SessionId::new("sid-xyz");
        let a = session_images_dir(Some(&id), std::path::Path::new("/origin/a")).unwrap();
        let b = session_images_dir(Some(&id), std::path::Path::new("/origin/b")).unwrap();
        assert_ne!(
            a, b,
            "same id under different cwd must map to different image dirs"
        );
        assert!(a.ends_with(std::path::Path::new("sid-xyz").join("images")));
        assert!(session_images_dir(None, std::path::Path::new("/x")).is_none());
    }

    #[test]
    fn strip_verbatim_prefix_normalizes_paths() {
        let strip = |s| strip_verbatim_prefix(std::path::Path::new(s));
        assert_eq!(strip(r"\\?\C:\x\1.jpg"), PathBuf::from(r"C:\x\1.jpg"));
        assert_eq!(
            strip(r"\\?\UNC\srv\s\1.jpg"),
            PathBuf::from(r"\\srv\s\1.jpg")
        );
        // Plain paths (the only form off Windows) pass through untouched.
        assert_eq!(strip("/Users/k/1.jpg"), PathBuf::from("/Users/k/1.jpg"));
    }

    // Helper to create a `PastedImage` with minimal required fields.
    fn make_image(element_id: u64, display_number: usize) -> PastedImage {
        PastedImage {
            element_id: ElementId::from_raw(element_id),
            display_number,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: 1024,
            encoded_bytes: None,
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        }
    }

    // ----- display_text ---------------------------------------------------

    #[test]
    fn display_text_format() {
        assert_eq!(display_text(1), "[Image #1]");
        assert_eq!(display_text(5), "[Image #5]");
        assert_eq!(display_text(10), "[Image #10]");
    }

    // ----- extension_for_mime ---------------------------------------------

    #[test]
    fn extension_for_known_mimes() {
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("image/tiff"), "tiff");
    }

    #[test]
    fn extension_for_unknown_mime() {
        assert_eq!(extension_for_mime("application/octet-stream"), "bin");
    }

    fn make_persistable_image(bytes: Vec<u8>) -> PastedImage {
        PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: bytes.len(),
            encoded_bytes: Some(Arc::from(bytes)),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        }
    }

    /// `persist_to_session` is atomic (write tmp + rename) and clears
    /// `encoded_bytes` after success.
    #[test]
    fn persist_to_session_writes_full_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"\x89PNG\r\n\x1a\nfake-but-recognizable-payload".to_vec();
        let mut img = make_persistable_image(payload.clone());
        persist_to_session(&mut img, dir.path()).expect("persist succeeded");
        let path = img.session_image_path.as_ref().expect("path set");
        let on_disk = std::fs::read(path).expect("readable");
        assert_eq!(on_disk, payload);
        assert!(img.encoded_bytes.is_none(), "in-memory bytes released");
        // No .tmp left behind after success. `Path::extension()` returns
        // `OsStr("tmp")` without the dot, so check filenames directly.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leaked tmp files: {leftovers:?}");
    }

    /// On `File::create` failure (read-only dir) `persist_to_session`
    /// returns `Err` and leaves no `.tmp` behind.
    #[cfg(unix)]
    #[test]
    fn persist_cleans_up_tmp_on_create_failure() {
        use std::os::unix::fs::PermissionsExt;
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut img = make_persistable_image(b"abc".to_vec());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = persist_to_session(&mut img, dir.path());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp leaked: {leftovers:?}");
    }

    /// Integration: persist → load_for_send round-trips the bytes.
    #[test]
    fn persist_then_load_for_send_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"\x89PNG\r\n\x1a\nround-trip-bytes-12345".to_vec();
        let mut img = make_persistable_image(payload.clone());
        persist_to_session(&mut img, dir.path()).expect("persist");
        let (bytes, mime) = load_for_send(&img).expect("load");
        assert_eq!(bytes, payload);
        assert_eq!(mime, "image/png");
    }

    /// `encoded_bytes.is_none()` is an error.
    #[test]
    fn persist_no_bytes_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = make_persistable_image(b"x".to_vec());
        img.encoded_bytes = None;
        let err = persist_to_session(&mut img, dir.path()).unwrap_err();
        assert!(err.to_string().contains("no encoded bytes"));
    }

    #[test]
    fn persist_preserves_original_source_path_for_file_paste() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = make_persistable_image(b"abc".to_vec());
        let original_source = std::path::PathBuf::from("/tmp/ephemeral/original.png");
        img.source_path = Some(original_source.clone());
        persist_to_session(&mut img, dir.path()).expect("persist");
        assert_eq!(img.source_path.as_ref(), Some(&original_source));
        assert_ne!(
            img.source_path.as_ref(),
            img.session_image_path.as_ref(),
            "display and durable paths have distinct ownership"
        );
    }

    /// Write into a path that cannot be created returns `Err`.
    #[cfg(unix)]
    #[test]
    fn persist_readonly_dir_returns_err() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses read-only mode; skip.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let readonly = dir.path().join("ro");
        std::fs::create_dir(&readonly).unwrap();
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut img = make_persistable_image(b"abc".to_vec());
        let res = persist_to_session(&mut img, &readonly);
        let _ = std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o700));
        assert!(res.is_err(), "expected write to read-only dir to fail");
    }

    // ----- shell_unescape -------------------------------------------------

    #[test]
    fn shell_unescape_spaces() {
        assert_eq!(
            shell_unescape(r"/path/to/my\ file.png"),
            "/path/to/my file.png"
        );
    }

    #[test]
    fn shell_unescape_parens() {
        assert_eq!(
            shell_unescape(r"/Downloads/screenshot\ \(2\).png"),
            "/Downloads/screenshot (2).png"
        );
    }

    #[test]
    fn shell_unescape_no_escapes() {
        assert_eq!(shell_unescape("/simple/path.png"), "/simple/path.png");
    }

    #[test]
    fn shell_unescape_trailing_backslash() {
        assert_eq!(shell_unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn shell_unescape_literal_backslash() {
        assert_eq!(shell_unescape(r"path\\name"), r"path\name");
    }

    // ----- shell_unescape / Windows-path round-trip ----------------------
    //
    // `\` is a path separator on Windows, not a shell escape. The
    // unescape must skip Windows-looking inputs or it would collapse
    // `C:\Users\Alice\image.png` to `C:UsersAliceimage.png`.

    #[test]
    fn shell_unescape_preserves_windows_drive_letter() {
        assert_eq!(
            shell_unescape(r"C:\Users\Alice\image.png"),
            r"C:\Users\Alice\image.png"
        );
        // Forward-slash drive paths (MSYS2 / MinGW) also pass through.
        assert_eq!(shell_unescape("C:/x/y.png"), "C:/x/y.png");
    }

    #[test]
    fn shell_unescape_preserves_windows_unc() {
        assert_eq!(
            shell_unescape(r"\\server\share\image.png"),
            r"\\server\share\image.png"
        );
    }

    #[test]
    fn looks_like_windows_path_positive_cases() {
        assert!(looks_like_windows_path(r"C:\Users\Alice"));
        assert!(looks_like_windows_path(r"d:\Downloads"));
        assert!(looks_like_windows_path("C:/Users/Alice"));
        assert!(looks_like_windows_path(r"\\server\share\file.png"));
        assert!(looks_like_windows_path(r"\\?\C:\long\path"));
    }

    #[test]
    fn looks_like_windows_path_negative_cases() {
        assert!(!looks_like_windows_path("/Users/Alice/file.png"));
        assert!(!looks_like_windows_path("~/Downloads/file.png"));
        assert!(!looks_like_windows_path("file://path"));
        assert!(!looks_like_windows_path("C:"));
        assert!(!looks_like_windows_path("C:foo"));
        assert!(!looks_like_windows_path(r"\foo"));
        assert!(!looks_like_windows_path(""));
    }

    #[test]
    fn token_to_path_round_trips_quoted_windows_path() {
        // Windows Terminal wraps paths-with-spaces in double quotes;
        // quote stripping then shell_unescape-skip must leave the
        // path intact for downstream `is_file()`.
        let path = token_to_path("\"C:\\Users\\Alice\\My Folder\\image.png\"").unwrap();
        assert_eq!(
            path,
            std::path::PathBuf::from(r"C:\Users\Alice\My Folder\image.png")
        );
    }

    // ----- try_read_image_from_path ----------------------------------------

    #[test]
    fn try_read_image_with_escaped_parens() {
        let dir = tempfile::tempdir().unwrap();
        let real_path = dir.path().join("screenshot (2).png");
        let png = make_test_png(10, 10);
        std::fs::write(&real_path, &png).unwrap();

        // Simulate what the terminal pastes: escaped spaces and parens
        let escaped = format!("{}/screenshot\\ \\(2\\).png", dir.path().display());
        let result = try_read_image_from_path(&escaped);
        assert!(
            result.is_some(),
            "should recognize path with escaped parens"
        );
        let img = result.unwrap();
        assert_eq!(img.mime_type, "image/png");
        assert!(
            img.source_path.is_some(),
            "source_path should be set for file-path images"
        );
    }

    #[test]
    fn try_read_image_with_escaped_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let real_path = dir.path().join("my file.png");
        let png = make_test_png(10, 10);
        std::fs::write(&real_path, &png).unwrap();

        let escaped = format!("{}/my\\ file.png", dir.path().display());
        let result = try_read_image_from_path(&escaped);
        assert!(result.is_some(), "should recognize path with escaped space");
        assert!(result.unwrap().source_path.is_some());
    }

    // ----- single-file resilience (drop with trailing whitespace / quotes /
    //       file:// URLs) ---------------------------------------------------

    /// Writes a real PNG at `path`. Helper to keep the multi-file tests tidy.
    fn write_png(path: &std::path::Path, w: u32, h: u32) {
        std::fs::write(path, make_test_png(w, h)).unwrap();
    }

    #[test]
    fn try_read_image_bare_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let result = try_read_image_from_path(&p.display().to_string()).unwrap();
        assert!(result.source_path.is_some());
        assert_eq!(result.mime_type, "image/png");
    }

    #[test]
    fn try_read_image_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let pasted = format!("{}\n", p.display());
        assert!(
            try_read_image_from_path(&pasted).is_some(),
            "single trailing newline must not break drop"
        );
    }

    #[test]
    fn try_read_image_trailing_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let pasted = format!("{}\r\n", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    #[test]
    fn try_read_image_double_quoted_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let pasted = format!("\"{}\"", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    #[test]
    fn try_read_image_single_quoted_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let pasted = format!("'{}'", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    // ----- file:// URL parsing -------------------------------------------

    #[test]
    fn try_read_image_file_url() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let url = format!("file://{}", p.display());
        assert!(try_read_image_from_path(&url).is_some());
    }

    #[test]
    fn try_read_image_file_url_percent_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("has space.png");
        write_png(&p, 2, 2);

        // Build a file:// URL with %20 in place of the literal space.
        let url = format!(
            "file://{}/has%20space.png",
            dir.path().display().to_string().replace(' ', "%20")
        );
        assert!(
            try_read_image_from_path(&url).is_some(),
            "percent-encoded file:// URL must decode to a real path"
        );
    }

    #[test]
    fn try_read_image_file_url_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let pasted = format!("file://{}\n", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    #[test]
    fn try_read_image_file_url_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        let pasted = format!("\"file://{}\"", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    // ----- multi-file drop -----------------------------------------------

    /// Non-image paths are canonicalized before insertion.
    fn canon(p: &std::path::Path) -> PathBuf {
        dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    #[test]
    fn multi_file_newline_separated() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);

        let pasted = format!("{}\n{}", a.display(), b.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].source_path.as_ref().unwrap(), &a);
        assert_eq!(images[1].source_path.as_ref().unwrap(), &b);
    }

    #[test]
    fn multi_file_space_separated_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);

        let pasted = format!("{} {}", a.display(), b.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].source_path.as_ref().unwrap(), &a);
        assert_eq!(images[1].source_path.as_ref().unwrap(), &b);
    }

    #[test]
    fn multi_file_mixed_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        let c = dir.path().join("c.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);
        write_png(&c, 2, 2);

        let pasted = format!("{}\r\n{}\n{}", a.display(), b.display(), c.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 3);
    }

    #[test]
    fn multi_file_file_urls_and_bare_paths_interleaved() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        let c = dir.path().join("c.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);
        write_png(&c, 2, 2);

        let pasted = format!(
            "file://{}\n{}\nfile://{}",
            a.display(),
            b.display(),
            c.display()
        );
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 3);
    }

    /// The whole-paste-or-nothing rule (preserves prose) causes the
    /// *entire* paste to fall through to plain text when any line
    /// fails to resolve. A non-image text file IS still a valid
    /// drop (it becomes a NonImage entry) — but a truly missing
    /// path breaks the batch.
    #[test]
    fn multi_file_missing_middle_path_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let c = dir.path().join("c.png");
        write_png(&a, 2, 2);
        write_png(&c, 2, 2);
        let missing = dir.path().join("missing_zzz.png");

        let pasted = format!("{}\n{}\n{}", a.display(), missing.display(), c.display());
        let images = try_read_images_from_paste(&pasted);
        assert!(
            images.is_empty(),
            "missing middle path must cause whole-paste fall-through; \
             got {images:?}",
        );
    }

    /// A non-image file in the middle IS a valid drop (becomes
    /// `NonImage`), so the batch still produces 2 images plus 1
    /// NonImage path — nothing is silently lost.
    #[test]
    fn multi_file_non_image_middle_still_emits_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let txt = dir.path().join("notes.txt");
        let c = dir.path().join("c.png");
        write_png(&a, 2, 2);
        std::fs::write(&txt, b"not an image").unwrap();
        write_png(&c, 2, 2);

        let pasted = format!("{}\n{}\n{}", a.display(), txt.display(), c.display());
        let entries = dropped_paths(&pasted);
        assert_eq!(entries.len(), 3);
        // Pin the source-order of the variants, not just the
        // counts. The drop classifier inserts in source order and
        // order determines the final prompt layout — a regression
        // that scrambled the order would still pass a count-only
        // assertion.
        assert!(
            matches!(entries[0], DroppedPath::Image(_)),
            "entries[0] must be Image; got {:?}",
            entries[0],
        );
        assert!(
            matches!(entries[1], DroppedPath::NonImage(_)),
            "entries[1] must be NonImage; got {:?}",
            entries[1],
        );
        assert!(
            matches!(entries[2], DroppedPath::Image(_)),
            "entries[2] must be Image; got {:?}",
            entries[2],
        );
    }

    // ----- negatives — must not auto-attach ------------------------------

    #[test]
    fn free_prose_containing_slash_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        // The path exists, but the surrounding prose attaches extra
        // tokens that prevent the path from validating.
        let pasted = format!("check out {} for the bug", p.display());
        let images = try_read_images_from_paste(&pasted);
        assert!(
            images.is_empty(),
            "free prose containing an image path must not auto-attach"
        );
    }

    #[test]
    fn missing_single_path_returns_empty() {
        let images = try_read_images_from_paste("/tmp/does_not_exist_zzz.png");
        assert!(images.is_empty());
        assert!(try_read_image_from_path("/tmp/does_not_exist_zzz.png").is_none());
    }

    #[test]
    fn relative_path_returns_empty() {
        // Relative path "foo.png" is not a real file at the test cwd.
        assert!(try_read_images_from_paste("foo.png").is_empty());
        assert!(try_read_image_from_path("foo.png").is_none());
    }

    #[test]
    fn random_multi_line_text_returns_empty() {
        assert!(try_read_images_from_paste("line one\nline two").is_empty());
    }

    // ----- additional edge cases ----

    #[test]
    fn bash_mode_prefix_not_treated_as_image() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        // "! /tmp/foo.png" is a bash-mode prefix paste, not a 2-token
        // file drop. The all-parts-anchored gate must reject the split
        // and leave the whole payload as one (invalid) token so the
        // caller can detect the `! ` prefix.
        let pasted = format!("! {}", p.display());
        assert!(try_read_images_from_paste(&pasted).is_empty());
    }

    #[test]
    fn prose_ending_in_png_is_not_attached() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join("foo.png");
        let bar = dir.path().join("bar.png");
        write_png(&foo, 2, 2);
        write_png(&bar, 2, 2);

        // Prose that *ends* with an image extension is a footgun.
        // With the all-parts-anchored gate, the first part
        // "see" doesn't anchor and we fall back to a single token that
        // fails validation as a path.
        let pasted = format!("see {} referenced in {}", foo.display(), bar.display());
        assert!(try_read_images_from_paste(&pasted).is_empty());
    }

    #[test]
    fn leading_space_before_single_path_attaches() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        // Leading space followed by a path produces a split-empty + path
        // pair; after trim+filter the parts collapse to a single token
        // and the path still attaches. Pins down this behavior.
        let pasted = format!(" {}", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    #[test]
    fn newline_wins_space_inside_line_not_split() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let bc = dir.path().join("b.png c.png"); // a single file with a space in its name
        let other = dir.path().join("d.png");
        write_png(&a, 2, 2);
        write_png(&bc, 2, 2);
        write_png(&other, 2, 2);

        // Mixed payload: newline-split wins; the second line is NOT
        // further space-split, so "b.png c.png" is one filename. Pins
        // down the "newline wins, space is only a single-line fallback"
        // rule.
        let pasted = format!("{}\n{}\n{}", a.display(), bc.display(), other.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(
            images.len(),
            3,
            "second line must be treated as a single filename with a space"
        );
    }

    #[test]
    fn quoted_path_with_internal_backslash_escape() {
        let dir = tempfile::tempdir().unwrap();
        // Inside the quotes the user supplied a backslash escape. Our
        // code still runs `shell_unescape` after stripping quotes — this
        // diverges from POSIX shell semantics (where backslash inside
        // double quotes is mostly literal) but is consistent with the
        // pre-existing single-image wrapper and the common drag-and-drop
        // flow. The test pins down the actual behavior.
        let p = dir.path().join("my file.png");
        write_png(&p, 2, 2);
        let pasted = format!("\"{}/my\\ file.png\"", dir.path().display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    // ----- file:// URL edge cases ----------------------------

    #[test]
    fn file_url_with_localhost_host() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        // `file://localhost/...` is accepted by the `url` crate and
        // yields the same local path as `file:///...`. Pins behavior.
        let pasted = format!("file://localhost{}", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    #[test]
    fn file_url_with_query_string_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        // `url::Url::to_file_path()` on `file:///…/foo.png?q=1` strips
        // the query, so the file is found and the image attaches. Pins
        // the current `url` crate contract; a future change that, say,
        // started including the query in the path component would trip
        // this assertion.
        let pasted = format!("file://{}?q=1", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    #[test]
    fn file_url_with_fragment_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("foo.png");
        write_png(&p, 2, 2);

        // Fragments are stripped by `url::Url::to_file_path()`, so this
        // resolves to the same local path. Pins behavior.
        let pasted = format!("file://{}#frag", p.display());
        assert!(try_read_image_from_path(&pasted).is_some());
    }

    // ----- multi-file space-separated mixed file:// + bare ---

    #[test]
    fn multi_file_space_separated_file_url_then_bare() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);

        let pasted = format!("file://{} {}", a.display(), b.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 2, "file:// + bare path split must work");
        assert_eq!(images[0].source_path.as_ref().unwrap(), &a);
        assert_eq!(images[1].source_path.as_ref().unwrap(), &b);
    }

    #[test]
    fn multi_file_space_separated_bare_then_file_url() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);

        let pasted = format!("{} file://{}", a.display(), b.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 2, "bare + file:// path split must work");
        assert_eq!(images[0].source_path.as_ref().unwrap(), &a);
        assert_eq!(images[1].source_path.as_ref().unwrap(), &b);
    }

    #[test]
    fn multi_file_mixed_newline_and_space_separated() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        let c = dir.path().join("c.png");
        let d = dir.path().join("d.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);
        write_png(&c, 2, 2);
        write_png(&d, 2, 2);

        let pasted = format!(
            "{} {}\n{} {}",
            a.display(),
            b.display(),
            c.display(),
            d.display()
        );
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 4, "mixed newline+space must flatten to 4");
        assert_eq!(images[0].source_path.as_ref().unwrap(), &a);
        assert_eq!(images[1].source_path.as_ref().unwrap(), &b);
        assert_eq!(images[2].source_path.as_ref().unwrap(), &c);
        assert_eq!(images[3].source_path.as_ref().unwrap(), &d);
    }

    /// A valid drop line followed by a prose comment line causes
    /// the whole paste to fall through to plain text — the dominant
    /// real-world use case is "screenshot URL + caption" and the
    /// caption must survive verbatim instead of being silently
    /// dropped on the floor.
    #[test]
    fn multi_file_valid_line_plus_prose_line_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);

        let pasted = format!(
            "{} {}\nbut please ignore the second screenshot",
            a.display(),
            b.display()
        );
        let images = try_read_images_from_paste(&pasted);
        assert!(
            images.is_empty(),
            "valid-drop-plus-prose paste must fall through so the prose \
             survives via plain-text-paste; got {images:?}",
        );
    }

    #[test]
    fn multi_file_space_separated_both_file_urls() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        write_png(&a, 2, 2);
        write_png(&b, 2, 2);

        let pasted = format!("file://{} file://{}", a.display(), b.display());
        let images = try_read_images_from_paste(&pasted);
        assert_eq!(images.len(), 2, "two file:// URLs must split on space");
        assert_eq!(images[0].source_path.as_ref().unwrap(), &a);
        assert_eq!(images[1].source_path.as_ref().unwrap(), &b);
    }

    #[test]
    fn file_url_single_slash_rejected_at_anchor_gate() {
        // Single-slash `file:` URLs lack the `file://` prefix the
        // anchor gate requires, and don't start with a path
        // anchor (`/`, `~/`, `X:\`) either — `file:` starts with the
        // ASCII letter `f`. So they fall through to plain text paste
        // regardless of filesystem state. Pins observable wrapper
        // behaviour against accidentally relaxing the anchor gate to
        // also accept `file:` (single-slash). A `url`-crate upgrade
        // that started or stopped accepting `file:/...` at parse
        // time would still satisfy this assertion because the
        // anchor gate now rejects single-slash strings *before* any
        // URL parsing runs.
        let pasted = "file:/tmp/should_not_exist_abc_echo_agent_pager.png";
        assert!(try_read_image_from_path(pasted).is_none());
    }

    #[test]
    fn file_url_lookalike_scheme_rejected() {
        // `file_url://` does NOT start with the literal `file://`, so
        // the URL branch is skipped and the bare-path branch runs;
        // the resulting "path" can't be on disk → None.
        let pasted = "file_url:///tmp/foo.png";
        assert!(try_read_image_from_path(pasted).is_none());
    }

    // ----- try_read_dropped_paths -----------------------------------------

    fn dropped_paths(text: &str) -> Vec<DroppedPath> {
        try_read_dropped_paths(text)
    }

    fn dropped_image_paths(text: &str) -> Vec<PathBuf> {
        dropped_paths(text)
            .into_iter()
            .filter_map(|d| match d {
                DroppedPath::Image(img) => img.source_path,
                _ => None,
            })
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn image_source_path_preserves_user_visible_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.png");
        let visible = dir.path().join("visible.png");
        write_png(&target, 8, 8);
        std::os::unix::fs::symlink(&target, &visible).unwrap();

        let images = try_read_images_from_paste(&visible.display().to_string());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].source_path.as_deref(), Some(visible.as_path()));
        assert_ne!(visible, dunce::canonicalize(&visible).unwrap());
    }

    fn dropped_non_image_paths(text: &str) -> Vec<PathBuf> {
        dropped_paths(text)
            .into_iter()
            .filter_map(|d| match d {
                DroppedPath::NonImage(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dropped_path_non_image_file_url_returns_decoded_path() {
        // A `file://` URL pointing at a non-image file should produce a
        // NonImage entry with the decoded absolute path — not be silently
        // ignored or routed to an image chip.
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, b"hello").unwrap();

        let url = format!("file://{}", txt.display());
        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1);
        assert_eq!(non_images[0], canon(&txt));
        // No image chip should be created for a .txt file.
        assert!(dropped_image_paths(&url).is_empty());
    }

    #[test]
    fn dropped_path_non_image_bare_path_returns_decoded_path() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("data.json");
        std::fs::write(&txt, b"{}").unwrap();

        let non_images = dropped_non_image_paths(&txt.display().to_string());
        assert_eq!(non_images.len(), 1);
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn paste_anchor_kind_classifies_each_form() {
        assert_eq!(paste_anchor_kind("file:///tmp/x.png"), "file_url");
        assert_eq!(paste_anchor_kind("/tmp/x.png"), "absolute");
        assert_eq!(paste_anchor_kind("~/Downloads/x.png"), "tilde");
        assert_eq!(
            paste_anchor_kind("C:\\Users\\Alice\\x.png"),
            "windows_drive"
        );
        assert_eq!(paste_anchor_kind("hello world"), "none");
        assert_eq!(paste_anchor_kind(""), "none");
        // ~ without slash is just prose, not a tilde anchor.
        assert_eq!(paste_anchor_kind("~lonely"), "none");
        // Lowercase Windows drive letters also accepted.
        assert_eq!(paste_anchor_kind("d:\\path"), "windows_drive");
        // Windows UNC paths (`\\server\share\...`) get their own bucket.
        assert_eq!(paste_anchor_kind(r"\\server\share\x.png"), "windows_unc");
    }

    #[test]
    fn starts_with_path_anchor_accepts_unc() {
        assert!(starts_with_path_anchor(r"\\server\share\file.png"));
        assert!(starts_with_path_anchor(r"\\?\C:\Users\Alice"));
        // Single backslash is not a UNC prefix and not a path anchor.
        assert!(!starts_with_path_anchor(r"\foo"));
    }

    #[test]
    fn dropped_path_image_file_url_returns_image() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("pic.png");
        write_png(&img, 2, 2);

        let url = format!("file://{}", img.display());
        let images = dropped_image_paths(&url);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0], img);
        assert!(dropped_non_image_paths(&url).is_empty());
    }

    #[test]
    fn dropped_path_percent_encoded_space_round_trips() {
        // `%20` must decode to a real space; the path with spaces must
        // survive intact (no truncation at the space).
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("My Documents");
        std::fs::create_dir_all(&sub).unwrap();
        let txt = sub.join("notes report.txt");
        std::fs::write(&txt, b"x").unwrap();

        // Construct the `file://` URL with `%20` for every space.
        let encoded = txt.display().to_string().replace(' ', "%20");
        let url = format!("file://{}", encoded);

        let non_images = dropped_non_image_paths(&url);
        assert_eq!(
            non_images.len(),
            1,
            "percent-encoded space must round-trip; got {non_images:?}"
        );
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_percent_encoded_hash_round_trips() {
        // `%23` decodes to `#`. The URL parser must not treat the
        // suffix as a fragment.
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notes#draft.txt");
        std::fs::write(&txt, b"x").unwrap();

        let encoded = txt.display().to_string().replace('#', "%23");
        let url = format!("file://{}", encoded);

        let non_images = dropped_non_image_paths(&url);
        assert_eq!(
            non_images.len(),
            1,
            "percent-encoded `#` must round-trip; got {non_images:?}"
        );
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_percent_encoded_question_round_trips() {
        // `%3F` decodes to `?`. The URL parser must not treat the
        // suffix as a query string.
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("query?file.txt");
        std::fs::write(&txt, b"x").unwrap();

        let encoded = txt.display().to_string().replace('?', "%3F");
        let url = format!("file://{}", encoded);

        let non_images = dropped_non_image_paths(&url);
        assert_eq!(
            non_images.len(),
            1,
            "percent-encoded `?` must round-trip; got {non_images:?}"
        );
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_multi_file_mixed_image_and_non_image() {
        // Drop one image + one text file. Image should become an
        // image chip, non-image should become a decoded path —
        // *neither* should be silently dropped on the floor.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        let txt = dir.path().join("b.txt");
        write_png(&png, 2, 2);
        std::fs::write(&txt, b"hi").unwrap();

        let pasted = format!("{}\n{}", png.display(), txt.display());
        let entries = dropped_paths(&pasted);
        assert_eq!(entries.len(), 2, "both entries must be reported");

        let mut saw_image = false;
        let mut saw_non_image = false;
        for entry in entries {
            match entry {
                DroppedPath::Image(img) => {
                    assert_eq!(img.source_path.as_ref().unwrap(), &png);
                    saw_image = true;
                }
                DroppedPath::NonImage(p) => {
                    assert_eq!(p, canon(&txt));
                    saw_non_image = true;
                }
            }
        }
        assert!(saw_image && saw_non_image);
    }

    #[test]
    fn dropped_path_multi_file_url_mixed() {
        // Same as above but both tokens are `file://` URLs.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("img.png");
        let txt = dir.path().join("doc.md");
        write_png(&png, 2, 2);
        std::fs::write(&txt, b"# hi").unwrap();

        let pasted = format!("file://{} file://{}", png.display(), txt.display());
        let entries = dropped_paths(&pasted);
        assert_eq!(entries.len(), 2);
        // Pin the variant of each token — a regression that collapses
        // both into Image (or both into NonImage) would otherwise be
        // missed by a bare `len() == 2` assertion.
        let mut saw_image = false;
        let mut saw_non_image = false;
        for entry in entries {
            match entry {
                DroppedPath::Image(img) => {
                    assert_eq!(img.source_path.as_ref().unwrap(), &png);
                    saw_image = true;
                }
                DroppedPath::NonImage(p) => {
                    assert_eq!(p, canon(&txt));
                    saw_non_image = true;
                }
            }
        }
        assert!(saw_image && saw_non_image);
    }

    #[test]
    fn dropped_path_two_non_image_file_urls_both_intercepted() {
        // The most likely real-world Finder multi-select drop pattern
        // for source code review: drag two text files into the TUI.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.md");
        let b = dir.path().join("b.md");
        std::fs::write(&a, b"# a").unwrap();
        std::fs::write(&b, b"# b").unwrap();

        let pasted = format!("file://{}\nfile://{}", a.display(), b.display());
        let entries = dropped_paths(&pasted);
        assert_eq!(entries.len(), 2);
        let paths: Vec<_> = entries
            .into_iter()
            .map(|d| match d {
                DroppedPath::NonImage(p) => p,
                _ => panic!("expected both as NonImage"),
            })
            .collect();
        assert_eq!(paths[0], canon(&a));
        assert_eq!(paths[1], canon(&b));
    }

    #[test]
    fn dropped_path_plus_sign_not_decoded_as_space() {
        // RFC 3986 path-style decoding preserves `+`; only
        // application/x-www-form-urlencoded decoding maps `+` → ` `.
        // Filenames with `+` are common (`C++ Source.cpp`, `5+5.txt`).
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("c++ source.cpp");
        std::fs::write(&txt, b"int main() {}").unwrap();

        // No percent-encoding: literal `+` in the URL.
        // (Spaces still need to be encoded.)
        let encoded = txt.display().to_string().replace(' ', "%20");
        let url = format!("file://{}", encoded);

        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1, "got {non_images:?}");
        let got = &non_images[0];
        assert_eq!(got, &canon(&txt));
        // Belt-and-braces: the decoded path must still contain a `+`
        // character, not a stray space.
        assert!(
            got.to_string_lossy().contains('+'),
            "`+` must survive decoding intact; got {got:?}"
        );
    }

    /// Build a `file://` URL by URL-encoding `dir.path()` as a path
    /// segment first, then appending the (already-encoded) leaf. Used
    /// by tests so a `TMPDIR` resolving under a path with characters
    /// that need percent-encoding (e.g. `+` or space) doesn't yield a
    /// silently-unparseable URL.
    fn build_file_url(dir: &std::path::Path, encoded_leaf: &str) -> String {
        let base = url::Url::from_file_path(dir)
            .expect("tempdir path must round-trip through url::Url::from_file_path");
        let url = format!("{}/{}", base.as_str().trim_end_matches('/'), encoded_leaf);
        assert!(
            url::Url::parse(&url).is_ok(),
            "constructed URL must parse: {url}"
        );
        url
    }

    #[test]
    fn dropped_path_multibyte_utf8_percent_encoded_round_trips() {
        // macOS Finder emits `%XX` triplets for each UTF-8 byte of
        // non-ASCII filename characters. `…` (U+2026) encodes as
        // `%E2%80%A6`. The full triplet sequence must decode to the
        // original codepoint, not be partially decoded or dropped.
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("ellipsis…file.md");
        std::fs::write(&txt, b"# hi").unwrap();

        let url = build_file_url(dir.path(), "ellipsis%E2%80%A6file.md");

        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1, "got {non_images:?}");
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_mixed_case_percent_hex_equivalent() {
        // RFC 3986 §2.1: `%2F` and `%2f` are equivalent. Some
        // producers emit lowercase, some uppercase — both must
        // round-trip to the same decoded path. Uses the ellipsis
        // codepoint `…` (UTF-8 bytes `E2 80 A6`) so the percent
        // triplets contain letters and case actually matters.
        let dir = tempfile::tempdir().unwrap();
        let ellipsis_file = dir.path().join("e…e.txt");
        std::fs::write(&ellipsis_file, b"x").unwrap();

        let upper = build_file_url(dir.path(), "e%E2%80%A6e.txt");
        let lower = build_file_url(dir.path(), "e%e2%80%a6e.txt");

        let up = dropped_non_image_paths(&upper);
        let lo = dropped_non_image_paths(&lower);
        assert_eq!(up, lo, "mixed-case %XX must decode identically");
        assert_eq!(up.len(), 1);
        assert_eq!(up[0], canon(&ellipsis_file));
    }

    #[test]
    fn dropped_path_invalid_percent_sequence_tolerated_outcome() {
        // `%ZZ` is not a valid percent escape. The workspace-pinned
        // `url` crate is lenient: it accepts the URL and `to_file_path`
        // returns the path with the literal `%ZZ` triplet preserved.
        // Two outcomes are acceptable: (a) the parser preserves the
        // literal `%ZZ` and we emit a single `NonImage` with `%ZZ` in
        // the path, or (b) the parser rejects the URL and we emit
        // empty Vec → caller falls through to plain text paste. Any
        // *third* outcome — empty Vec without falling through, or
        // partial decoding of the suffix — must fail the test.
        //
        // Hermeticity: build the URL under a tempfile so a hostile
        // `/tmp/bad%ZZname.txt` left around by a previous test can't
        // change the variant emitted.
        let dir = tempfile::tempdir().unwrap();
        let base = url::Url::from_file_path(dir.path()).unwrap();
        let url = format!("{}/bad%ZZname.txt", base.as_str().trim_end_matches('/'));
        let entries = dropped_paths(&url);
        let ok = entries.is_empty()
            || (entries.len() == 1
                && matches!(&entries[0], DroppedPath::NonImage(p) if p.to_string_lossy().contains("%ZZ")));
        assert!(
            ok,
            "%ZZ outcome must be either empty-Vec or single NonImage with literal `%ZZ`; got {entries:?}"
        );
    }

    #[test]
    fn dropped_path_extension_says_image_bytes_say_no_falls_to_non_image() {
        // `.png` extension but bytes are garbage — `read_image_at_path`
        // rejects via `mime_from_bytes` returning octet-stream, so the
        // NonImage gate fires and the user gets the path text instead
        // of a chip.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("corrupt.png");
        std::fs::write(&fake, b"this is not a PNG").unwrap();

        let url = format!("file://{}", fake.display());
        let entries = dropped_paths(&url);
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            DroppedPath::NonImage(p) => assert_eq!(p, &canon(&fake)),
            other => panic!("expected NonImage fallthrough, got {other:?}"),
        }
    }

    /// A bare cwd-relative image filename like `foo.png` (no `/`,
    /// no `~/`, no `X:\`, no `file://`) is rejected by the anchor
    /// gate BEFORE `read_image_at_path` runs — so even if `foo.png`
    /// happened to exist in the test process's cwd, it would not be
    /// intercepted as an Image. The contrast case (an absolute-path
    /// `file://` URL to the same content) is intercepted,
    /// documenting the asymmetry next to executable code.
    #[test]
    fn dropped_path_bare_cwd_relative_image_name_not_intercepted() {
        // Create a real PNG at <tempdir>/foo.png. The bare name
        // `foo.png` must NOT be intercepted: the anchor gate
        // short-circuits before `read_image_at_path` runs.
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("foo.png");
        write_png(&abs, 2, 2);

        // Bare relative name → rejected at the anchor gate.
        let bare = dropped_paths("foo.png");
        assert!(
            bare.is_empty(),
            "bare cwd-relative image name must NOT be intercepted; got {bare:?}"
        );

        // Same content via an absolute `file://` URL → accepted as Image.
        let url = format!("file://{}", abs.display());
        let via_url = dropped_paths(&url);
        assert_eq!(via_url.len(), 1);
        assert!(
            matches!(via_url[0], DroppedPath::Image(_)),
            "absolute file:// to the same PNG must be intercepted as Image; got {via_url:?}"
        );
    }

    /// `file:///` (empty path) and `file://` (root path) are
    /// pathological — never legitimate drop URIs. They must be
    /// rejected upstream of the existence check so the fallback
    /// branch can't emit a bogus `NonImage("/")` entry.
    #[test]
    fn file_url_with_empty_or_root_path_is_rejected() {
        // `file:///` parses to path `/` on Unix. Rejected by the
        // root-path gate.
        let entries = dropped_paths("file:///");
        assert!(
            entries.is_empty(),
            "file:/// must be rejected; got {entries:?}",
        );

        // `file://` (no path component) is also rejected — either
        // url-parse returns None for the empty path, or the empty-
        // OsStr gate catches it.
        let entries = dropped_paths("file://");
        assert!(
            entries.is_empty(),
            "file:// must be rejected; got {entries:?}",
        );
    }

    /// A `file://` URL with a percent-encoded NUL or embedded
    /// CR/LF byte decodes to a path that corrupts the terminal/text
    /// pipeline when inserted as text. Reject these at parse time
    /// so the prompt never sees them.
    ///
    /// The gate is intentionally narrow (NUL, CR, LF) — TAB and
    /// other low-control bytes are legal in Unix filenames and the
    /// TUI's text path renders them fine.
    #[test]
    fn file_url_with_nul_byte_path_is_rejected() {
        let entries = dropped_paths("file:///tmp/path%00.png");
        assert!(
            entries.is_empty(),
            "NUL-byte path must be rejected; got {entries:?}",
        );

        // CR and LF likewise corrupt the paste pipeline; reject them.
        let entries = dropped_paths("file:///tmp/cr%0Dpath.txt");
        assert!(
            entries.is_empty(),
            "CR-byte path must be rejected; got {entries:?}",
        );
        let entries = dropped_paths("file:///tmp/lf%0Apath.txt");
        assert!(
            entries.is_empty(),
            "LF-byte path must be rejected; got {entries:?}",
        );
    }

    /// HEIC/HEIF/AVIF/ICO are intentionally NOT in `IMAGE_EXTENSIONS`
    /// — the inline overlay doesn't render them, so we fall through
    /// to NonImage path text instead of falsely promoting a chip.
    #[test]
    fn unsupported_image_extensions_fall_to_non_image() {
        for ext in ["heic", "heif", "avif", "ico"] {
            assert!(!IMAGE_EXTENSIONS.contains(&ext), "{ext} must be omitted");
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("img.{ext}"));
            std::fs::write(&path, b"any bytes").unwrap();
            let url = format!("file://{}", path.display());
            let entries = dropped_paths(&url);
            assert_eq!(entries.len(), 1, "{ext}: {entries:?}");
            assert!(
                matches!(entries[0], DroppedPath::NonImage(_)),
                "{ext} must fall through to NonImage, got {:?}",
                entries[0],
            );
        }
    }

    /// SVG is intentionally NOT in `IMAGE_EXTENSIONS` (XML/text
    /// formats aren't sniffed as images and the inline overlay does
    /// not render SVG). An `.svg` drop must fall through to NonImage
    /// so the user gets a path string they can pass to the agent.
    #[test]
    fn svg_extension_not_intercepted_as_image() {
        assert!(!IMAGE_EXTENSIONS.contains(&"svg"));
        let dir = tempfile::tempdir().unwrap();
        let svg = dir.path().join("logo.svg");
        std::fs::write(
            &svg,
            br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"/>"#,
        )
        .unwrap();

        let url = format!("file://{}", svg.display());
        let entries = dropped_paths(&url);
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(entries[0], DroppedPath::NonImage(_)),
            "SVG must fall through to NonImage; got {:?}",
            entries[0],
        );
    }

    #[test]
    fn dropped_path_trailing_spaces_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("trail3.txt");
        std::fs::write(&txt, b"x").unwrap();

        let url = format!("file://{}   ", txt.display());
        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1, "got {non_images:?}");
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_trailing_tab_and_newline_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("trail4.txt");
        std::fs::write(&txt, b"x").unwrap();

        let url = format!("file://{}\t\n", txt.display());
        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1, "got {non_images:?}");
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_empty_line_between_file_urls_tolerated() {
        // Double newline between two `file://` URLs. `tokenize_paste`
        // is supposed to filter the empty intermediate token; pin
        // that for the `DroppedPath` flow.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("blank1.txt");
        let b = dir.path().join("blank2.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();

        let pasted = format!("file://{}\n\nfile://{}", a.display(), b.display());
        let non_images = dropped_non_image_paths(&pasted);
        assert_eq!(non_images.len(), 2, "got {non_images:?}");
        assert_eq!(non_images[0], canon(&a));
        assert_eq!(non_images[1], canon(&b));
    }

    #[test]
    fn dropped_path_double_space_between_file_urls_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("dbl1.txt");
        let b = dir.path().join("dbl2.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();

        let pasted = format!("file://{}  file://{}", a.display(), b.display());
        let non_images = dropped_non_image_paths(&pasted);
        assert_eq!(non_images.len(), 2, "got {non_images:?}");
        assert_eq!(non_images[0], canon(&a));
        assert_eq!(non_images[1], canon(&b));
    }

    #[test]
    fn dropped_path_directory_via_file_url_intercepted_as_non_image() {
        // A directory dropped as a `file://` URL is intercepted as
        // NonImage (the NonImage branch uses `path.exists()`, not
        // `is_file()`). Useful for "drag a folder into the TUI" UX.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a_folder");
        std::fs::create_dir_all(&sub).unwrap();

        let url = format!("file://{}", sub.display());
        let entries = dropped_paths(&url);
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], DroppedPath::NonImage(_)));
    }

    #[test]
    fn dropped_path_directory_via_bare_path_intercepted_as_non_image() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("another_folder");
        std::fs::create_dir_all(&sub).unwrap();

        let entries = dropped_paths(&sub.display().to_string());
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], DroppedPath::NonImage(_)));
    }

    #[test]
    fn dropped_path_prose_with_embedded_existing_path_not_truncated() {
        // /etc/hosts exists on every Unix-y machine. A sentence that
        // mentions it must NOT be truncated to just the path token.
        // (Regression sentinel for "prose-with-real-paths" — the
        // full sentence isn't a valid file, so the all-anchored
        // tokenizer gate falls back to a single-token line which
        // doesn't exist on disk and hence is rejected.)
        let entries = dropped_paths("I read /etc/passwd and got confused");
        assert!(
            entries.is_empty(),
            "prose with embedded existing path must fall through to plain text; got {entries:?}"
        );

        let entries = dropped_paths("Look at /etc/hosts to debug DNS");
        assert!(
            entries.is_empty(),
            "prose with embedded existing path must fall through to plain text; got {entries:?}"
        );
    }

    #[test]
    fn try_read_images_from_paste_equals_image_filtered_dropped_paths() {
        // `try_read_images_from_paste` is now a thin filter over
        // `try_read_dropped_paths`. Lock in the delegation invariant
        // for several input shapes so a regression that diverges
        // only in one shape (e.g. the empty-paste case) would still
        // break the public-API contract visibly.
        let dir = tempfile::tempdir().unwrap();
        let png1 = dir.path().join("img1.png");
        let png2 = dir.path().join("img2.png");
        let txt1 = dir.path().join("doc1.md");
        let txt2 = dir.path().join("doc2.md");
        write_png(&png1, 2, 2);
        write_png(&png2, 2, 2);
        std::fs::write(&txt1, b"# x").unwrap();
        std::fs::write(&txt2, b"# y").unwrap();

        // Cover: empty, prose, image-only, non-image-only,
        // mixed-image-and-non-image, multi-image, multi-non-image.
        let inputs: Vec<String> = vec![
            String::new(),
            "hello world this is just prose".to_string(),
            format!("file://{}", png1.display()),
            format!("file://{}", txt1.display()),
            format!("file://{} file://{}", png1.display(), txt1.display()),
            format!("file://{}\nfile://{}", png1.display(), png2.display()),
            format!("file://{}\nfile://{}", txt1.display(), txt2.display()),
        ];

        for input in &inputs {
            let images = try_read_images_from_paste(input);
            let dropped = try_read_dropped_paths(input);
            let image_paths_via_dropped: Vec<PathBuf> = dropped
                .into_iter()
                .filter_map(|d| match d {
                    DroppedPath::Image(img) => img.source_path,
                    DroppedPath::NonImage(_) => None,
                })
                .collect();
            let image_paths_via_filter: Vec<PathBuf> = images
                .into_iter()
                .filter_map(|img| img.source_path)
                .collect();
            assert_eq!(
                image_paths_via_filter, image_paths_via_dropped,
                "delegation invariant must hold for input {input:?}"
            );
        }
    }

    /// A single line that space-splits into multiple anchored
    /// tokens where some don't resolve must NOT silently emit only
    /// the resolving ones. The entire line falls through so the
    /// unresolved substring isn't dropped into the void.
    #[test]
    fn dropped_path_per_line_partial_resolution_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        // Hermeticity: `space_split_line` splits on each space that
        // precedes a drop anchor. A `$TMPDIR` containing a space
        // would inject extra split points into our `bogus` token and
        // break the intended two-token tokenisation. macOS' default
        // `/var/folders/...` and Linux' default `/tmp/...` are safe;
        // a CI sandbox with `TMPDIR=/some path/foo` is not. Fail
        // loudly rather than silently producing the wrong test
        // shape.
        assert!(
            !dir.path().to_string_lossy().contains(' '),
            "this test assumes no-space TMPDIR; got {:?}",
            dir.path()
        );
        let real = dir.path().join("real_dir");
        std::fs::create_dir_all(&real).unwrap();

        // Build a line that space-splits to [bogus_anchored, real_dir].
        // `bogus_anchored` is `<tmpdir>/nope nope nope` — anchored
        // (starts with `/`) so the all-anchored gate keeps it, but
        // doesn't exist on disk.
        let bogus = dir.path().join("nope nope nope");
        let pasted = format!("{} {}", bogus.display(), real.display());

        // bogus.exists() is false; real.exists() is true. After the
        // per-line all-or-nothing gate the line should emit *nothing*
        // so the caller falls through to plain text paste — the user
        // sees the verbatim string in the prompt rather than only
        // `real_dir` appearing.
        let entries = dropped_paths(&pasted);
        assert!(
            entries.is_empty(),
            "partial-resolution within a single line must fall through; got {entries:?}"
        );
    }

    /// A drop-line + prose-line paste falls through to plain text
    /// so the prose isn't silently lost — the screenshot-URL-
    /// plus-caption pattern is the dominant use case and naive
    /// per-line independence would eat the caption.
    #[test]
    fn dropped_path_url_plus_prose_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, b"x").unwrap();

        let pasted = format!(
            "file://{}\nbut please ignore the second comment line",
            real.display()
        );
        let entries = dropped_paths(&pasted);
        assert!(
            entries.is_empty(),
            "URL-plus-prose paste must fall through; got {entries:?}",
        );
    }

    /// The canonical case: a screenshot URL followed by a
    /// hand-typed caption must NOT lose the caption — the whole
    /// paste falls through to plain text.
    #[test]
    fn dropped_path_url_plus_caption_falls_through_for_plain_paste() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("screenshot.png");
        write_png(&png, 2, 2);

        let pasted = format!("file://{}\nplease look at this", png.display());
        let entries = dropped_paths(&pasted);
        assert!(
            entries.is_empty(),
            "screenshot URL + caption must fall through to plain text \
             paste; got {entries:?}",
        );
    }

    #[test]
    fn dropped_path_single_token_existing_bare_path_intercepted() {
        // The flip side of the prose test: a single bare anchored
        // path that exists IS a drop. Use a hermetic tempfile-backed
        // directory so the test doesn't depend on `/tmp` existing or
        // having any particular contents in a CI sandbox.
        let dir = tempfile::tempdir().unwrap();
        let entries = dropped_paths(&dir.path().display().to_string());
        // The directory exists; the NonImage gate uses `path.exists()`
        // (not `is_file()`), so directories qualify.
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], DroppedPath::NonImage(_)));
    }

    #[test]
    fn dropped_path_trailing_newline_tolerated_non_image() {
        // Trailing-newline tolerance must extend to non-image paths.
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("trail.txt");
        std::fs::write(&txt, b"x").unwrap();

        let url = format!("file://{}\n", txt.display());
        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1);
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_trailing_crlf_tolerated_non_image() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("trail2.txt");
        std::fs::write(&txt, b"x").unwrap();

        let url = format!("file://{}\r\n", txt.display());
        let non_images = dropped_non_image_paths(&url);
        assert_eq!(non_images.len(), 1);
        assert_eq!(non_images[0], canon(&txt));
    }

    #[test]
    fn dropped_path_prose_not_intercepted() {
        // Free-form text must NOT be intercepted as a drop path.
        let entries = dropped_paths("hello world this is just text");
        assert!(entries.is_empty());
    }

    #[test]
    fn dropped_path_nonexistent_bare_path_not_intercepted() {
        // A bare path to a non-existent file is NOT intercepted: the
        // user might just have typed `/etc/passwd` as part of prose.
        // (The image branch already filters on file existence; this
        // mirrors that for the non-image branch.)
        let entries = dropped_paths("/tmp/definitely_does_not_exist_xyz_echo_agent_pager.txt");
        assert!(
            entries.is_empty(),
            "bare nonexistent path must fall through to prose; got {entries:?}"
        );
    }

    /// Regression sentinel for the `canonicalize().unwrap_or(path)`
    /// fallback inside `try_read_dropped_path`. A `file://` URL is an
    /// unambiguous drop URI even when the target is missing (stale
    /// path, network mount). `canonicalize()` returns `Err` for
    /// missing targets — the fallback branch must emit the raw
    /// decoded path as `NonImage` so the user still gets a usable
    /// path string.
    #[test]
    fn dropped_path_nonexistent_file_url_still_intercepted() {
        // Use a tempdir-rooted nonexistent path so a developer's or
        // CI sandbox's filesystem can't accidentally make the path
        // resolve (`/tmp/definitely_does_not_exist...` could be
        // present on a noisy machine).
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("does/not/exist/at/all.txt");
        let url = format!("file://{}", nonexistent.display());
        let entries = dropped_paths(&url);
        assert_eq!(entries.len(), 1);
        // Explicit variant check first so a regression that
        // collapsed `NonImage` into `Image` (or returned no entries)
        // fails with a clean error rather than a destructuring panic.
        assert!(
            matches!(entries[0], DroppedPath::NonImage(_)),
            "expected NonImage variant; got {:?}",
            entries[0]
        );
        match &entries[0] {
            DroppedPath::NonImage(p) => {
                assert_eq!(p, &nonexistent);
            }
            _ => unreachable!("matches! above asserted the variant"),
        }
    }

    // ----- reconcile ------------------------------------------------------

    #[test]
    fn reconcile_keeps_live_images() {
        let mut images = vec![make_image(1, 1), make_image(2, 2), make_image(3, 3)];
        let live: HashSet<ElementId> = [ElementId::from_raw(1), ElementId::from_raw(3)].into();

        reconcile(&mut images, &live);

        assert_eq!(images.len(), 2);
        assert_eq!(images[0].display_number, 1);
        assert_eq!(images[1].display_number, 3);
    }

    #[test]
    fn reconcile_removes_all_when_empty_live_set() {
        let mut images = vec![make_image(1, 1), make_image(2, 2)];
        let live: HashSet<ElementId> = HashSet::new();

        reconcile(&mut images, &live);

        assert!(images.is_empty());
    }

    #[test]
    fn reconcile_noop_when_all_live() {
        let mut images = vec![make_image(1, 1), make_image(2, 2)];
        let live: HashSet<ElementId> = [ElementId::from_raw(1), ElementId::from_raw(2)].into();

        reconcile(&mut images, &live);

        assert_eq!(images.len(), 2);
    }

    #[test]
    fn reconcile_noop_on_empty_images() {
        let mut images: Vec<PastedImage> = Vec::new();
        let live: HashSet<ElementId> = [ElementId::from_raw(1)].into();

        reconcile(&mut images, &live);

        assert!(images.is_empty());
    }

    // ----- clear ----------------------------------------------------------

    #[test]
    fn clear_resets_images_and_counter() {
        let mut images = vec![make_image(1, 1), make_image(2, 2)];
        let mut counter = 5usize;

        clear(&mut images, &mut counter);

        assert!(images.is_empty());
        assert_eq!(counter, 0);
    }

    // ----- persist_to_session ------------------------------------------------

    #[test]
    fn persist_writes_file_and_clears_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let images_dir = dir.path().join("images");

        let png_bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        let mut img = PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: png_bytes.len(),
            encoded_bytes: Some(Arc::from(png_bytes.clone())),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        };

        persist_to_session(&mut img, &images_dir).unwrap();

        // File was written
        let path = img.session_image_path.as_ref().unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with(".png"));
        assert_eq!(std::fs::read(path).unwrap(), png_bytes);

        // In-memory bytes released
        assert!(img.encoded_bytes.is_none());

        // source_path stays None for clipboard pastes — the chip
        // should show `[Image #1]` without an internal path.
        assert!(
            img.source_path.is_none(),
            "clipboard paste source_path should remain None"
        );
    }

    #[test]
    fn persist_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let images_dir = dir.path().join("deep").join("nested").join("images");
        assert!(!images_dir.exists());

        let mut img = PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/jpeg".into(),
            dimensions: None,
            byte_len: 4,
            encoded_bytes: Some(Arc::from(vec![0xff, 0xd8, 0xff, 0xe0])),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        };

        persist_to_session(&mut img, &images_dir).unwrap();

        assert!(images_dir.exists());
        let path = img.session_image_path.as_ref().unwrap();
        assert!(path.to_string_lossy().ends_with(".jpg"));
    }

    #[test]
    fn persist_fails_without_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut img = make_image(1, 1); // encoded_bytes is None
        let result = persist_to_session(&mut img, dir.path());
        assert!(result.is_err());
    }

    // ----- persist_to_session path ownership --------------------------------

    #[test]
    fn persist_clipboard_image_keeps_source_path_none() {
        // Clipboard paste (Copy Image in browser/Slack): source_path
        // starts as None and should stay None after persistence so the
        // chip shows `[Image #1]` without an internal session path.
        let dir = tempfile::tempdir().unwrap();
        let images_dir = dir.path().join("images");

        let png = make_test_png(100, 80);
        let mut img = PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((100, 80)),
            byte_len: png.len(),
            encoded_bytes: Some(Arc::from(png)),
            source_path: None, // clipboard paste — no original path
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        };

        persist_to_session(&mut img, &images_dir).unwrap();

        // source_path stays None for clipboard pastes.
        assert!(
            img.source_path.is_none(),
            "clipboard paste should not gain a source_path"
        );

        // session_image_path is set (bytes are persisted for reload).
        assert!(img.session_image_path.is_some());

        // Display text shows no path.
        let text = display_text(1);
        assert_eq!(text, "[Image #1]");
    }

    #[test]
    fn persist_file_image_keeps_original_and_durable_paths_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let images_dir = dir.path().join("images");

        let png = make_test_png(50, 50);
        let original_path = PathBuf::from("/tmp/ephemeral-screenshot.png");
        let mut img = PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((50, 50)),
            byte_len: png.len(),
            encoded_bytes: Some(Arc::from(png)),
            source_path: Some(original_path.clone()),
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        };

        persist_to_session(&mut img, &images_dir).unwrap();

        let session_path = img.session_image_path.as_ref().unwrap();
        assert_eq!(img.source_path.as_ref(), Some(&original_path));
        assert_ne!(
            img.source_path.as_ref(),
            Some(session_path),
            "source_path is the original display path, not the durable copy"
        );
    }

    // ----- from_clipboard_data -----------------------------------------------

    #[test]
    fn from_clipboard_data_populates_fields() {
        let data = crate::clipboard::ImageData {
            data: vec![1, 2, 3, 4],
            mime_type: "image/png".into(),
        };
        let img = from_clipboard_data(&data);
        assert_eq!(img.mime_type, "image/png");
        assert_eq!(img.byte_len, 4);
        assert!(img.encoded_bytes.is_some());
        assert_eq!(img.encoded_bytes.as_ref().unwrap().len(), 4);
    }

    #[test]
    fn from_clipboard_prepares_preview_before_render() {
        let _guard = crate::terminal::image::set_protocol_for_test(
            crate::terminal::image::GraphicsProtocol::Kitty,
        );
        let data = crate::clipboard::ImageData {
            data: make_test_png(16, 12),
            mime_type: "image/png".into(),
        };

        let img = from_clipboard_data(&data);
        assert!(img.preview.is_pending());
        img.preview_preparation().unwrap().run();
        let (bytes, dimensions) = img.preview.prepared().expect("valid PNG becomes ready");
        assert_eq!(dimensions, (16, 12));
        assert!(crate::terminal::image::kitty_format_from_bytes(bytes).is_some());
    }

    #[test]
    fn from_clipboard_marks_corrupt_preview_failed_without_losing_send_bytes() {
        let _guard = crate::terminal::image::set_protocol_for_test(
            crate::terminal::image::GraphicsProtocol::Kitty,
        );
        let data = crate::clipboard::ImageData {
            data: b"\x89PNG\r\n\x1a\ncorrupt".to_vec(),
            mime_type: "image/png".into(),
        };

        let img = from_clipboard_data(&data);
        img.preview_preparation().unwrap().run();
        assert!(img.preview.is_failed());
        assert_eq!(img.encoded_bytes.as_deref(), Some(data.data.as_slice()));
    }

    // ----- test PNG helper --------------------------------------------------

    /// Generate a valid minimal PNG of the given dimensions.
    fn make_test_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(width, height, Rgba([128, 64, 32, 255]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn make_test_jpeg(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(width, height, Rgb([128, 64, 32]));
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        buf
    }

    /// Create a `PastedImage` with real PNG bytes.
    fn make_real_image(width: u32, height: u32) -> PastedImage {
        let png = make_test_png(width, height);
        PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((width, height)),
            byte_len: png.len(),
            encoded_bytes: Some(Arc::from(png)),
            source_path: None,
            staged_temp_path: None,
            session_image_path: None,
            preview: PromptImagePreview::default(),
        }
    }

    // ----- load_for_send ------------------------------------------------

    #[test]
    fn load_small_image_passes_through() {
        let img = make_real_image(100, 80);
        let original_len = img.encoded_bytes.as_ref().unwrap().len();
        let (bytes, mime) = load_for_send(&img).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes.len(), original_len);
    }

    #[test]
    fn load_rejects_image_below_eight_pixel_minimum() {
        let img = make_real_image(1, 1);
        assert!(load_for_send(&img).is_none());
    }

    #[test]
    fn load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        let png = make_test_png(50, 50);
        std::fs::write(&path, &png).unwrap();

        let img = PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((50, 50)),
            byte_len: png.len(),
            encoded_bytes: None, // bytes released
            source_path: None,
            staged_temp_path: None,
            session_image_path: Some(path),
            preview: PromptImagePreview::default(),
        };
        let (bytes, _) = load_for_send(&img).unwrap();
        assert_eq!(bytes.len(), png.len());
    }

    #[test]
    fn load_returns_none_for_missing_data() {
        let img = make_image(1, 1); // no bytes, no file
        assert!(load_for_send(&img).is_none());
    }

    // ----- build_content_blocks_with_workspace --------------------------------

    fn build_blocks_no_workspace(
        text: String,
        images: Vec<PastedImage>,
    ) -> Vec<agent_client_protocol::ContentBlock> {
        build_content_blocks_with_workspace(text, images, None)
    }

    #[test]
    fn build_blocks_text_only() {
        let blocks = build_blocks_no_workspace("hello".into(), vec![]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            agent_client_protocol::ContentBlock::Text(_)
        ));
    }

    #[test]
    fn build_blocks_with_in_memory_image() {
        let img = make_real_image(100, 80);
        let blocks = build_blocks_no_workspace("look at this [Image #1]".into(), vec![img]);
        assert_eq!(blocks.len(), 2);
        if let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] {
            assert_eq!(ic.mime_type, "image/png");
            assert!(!ic.data.is_empty());
            assert!(ic.uri.is_none());
        } else {
            panic!("expected Image block");
        }
    }

    #[test]
    fn build_blocks_with_file_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        let png = make_test_png(60, 40);
        std::fs::write(&path, &png).unwrap();

        let img = PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: Some((60, 40)),
            byte_len: png.len(),
            encoded_bytes: None,
            source_path: None,
            staged_temp_path: None,
            session_image_path: Some(path.clone()),
            preview: PromptImagePreview::default(),
        };
        let blocks = build_blocks_no_workspace("text".into(), vec![img]);
        assert_eq!(blocks.len(), 2);
        if let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] {
            assert!(!ic.data.is_empty());
            // The durable session copy is surfaced through `uri` even for
            // clipboard pastes (no `source_path`). This is the reference
            // `image_edit` resolves `[Image #N]` against; vision is
            // unaffected because `pick_user_image_url` never forwards a
            // `file://` URI to the model.
            assert_eq!(
                ic.uri.as_deref(),
                Some(format!("file://{}", path.display()).as_str()),
                "clipboard images must carry the durable session path as a file:// URI"
            );
        } else {
            panic!("expected Image block");
        }
    }

    #[test]
    fn build_blocks_omits_uri_for_stale_source_path() {
        let mut img = make_real_image(100, 80);
        img.source_path = Some(PathBuf::from("/Users/test/logo.png"));
        let blocks = build_blocks_no_workspace("text".into(), vec![img]);
        assert_eq!(blocks.len(), 2);
        if let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] {
            assert!(ic.uri.is_none());
            assert!(!ic.data.is_empty());
        } else {
            panic!("expected Image block");
        }
    }

    #[cfg(unix)]
    #[test]
    fn build_blocks_canonicalizes_source_only_for_wire_uri() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.png");
        let visible = dir.path().join("visible.png");
        std::fs::write(&target, make_test_png(8, 8)).unwrap();
        std::os::unix::fs::symlink(&target, &visible).unwrap();
        let mut img = make_real_image(8, 8);
        img.source_path = Some(visible.clone());

        let blocks = build_blocks_no_workspace("text".into(), vec![img]);
        let agent_client_protocol::ContentBlock::Image(image) = &blocks[1] else {
            panic!("expected image");
        };
        let canonical_target = dunce::canonicalize(&target).unwrap();
        assert_eq!(
            image.uri.as_deref(),
            Some(format!("file://{}", canonical_target.display()).as_str())
        );
        assert_ne!(visible, target);
    }

    #[test]
    fn build_blocks_sets_display_number_meta() {
        let mut img = make_real_image(40, 30);
        img.display_number = 3;
        let blocks = build_blocks_no_workspace("text [Image #3]".into(), vec![img]);
        assert_eq!(blocks.len(), 2);
        let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] else {
            panic!("expected Image block");
        };
        assert_eq!(
            echo_agent_shared::placeholder_images::display_number_from_meta(ic.meta.as_ref()),
            Some(3),
            "image block _meta must carry the real display number for token resolution"
        );
    }

    #[test]
    fn build_blocks_uri_prefers_durable_session_path() {
        let mut img = make_real_image(100, 80);
        img.source_path = Some(PathBuf::from("/Users/test/original.png"));
        img.session_image_path = Some(PathBuf::from("/Users/test/.echo-agent/session/image.png"));
        let blocks = build_blocks_no_workspace("text".into(), vec![img]);
        assert_eq!(blocks.len(), 2);
        if let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] {
            assert_eq!(
                ic.uri.as_deref(),
                Some("file:///Users/test/.echo-agent/session/image.png"),
                "model URI must prefer the durable session path"
            );
        } else {
            panic!("expected Image block");
        }
    }

    #[test]
    fn build_blocks_skips_missing_image() {
        let img = make_image(1, 1); // no bytes, no file path
        let blocks = build_blocks_no_workspace("text".into(), vec![img]);
        // Only the text block; image was skipped.
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn build_blocks_one_bad_one_good() {
        let bad = make_image(1, 1); // no bytes
        let good = make_real_image(50, 50);
        let blocks = build_blocks_no_workspace("text".into(), vec![bad, good]);
        // Text + 1 good image; bad image skipped.
        assert_eq!(blocks.len(), 2);
    }

    // ----- Orphan placeholder fallback ----------------------------------
    //
    // These tests go through `build_content_blocks_with_prefixes` with
    // an explicit hermetic prefix list, so they do NOT read the
    // ambient process `$HOME`. CI runners with unusual `HOME`
    // settings cannot flip the outcomes.

    #[test]
    fn build_blocks_orphan_placeholder_loaded_from_disk() {
        use base64::Engine as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orphan.png");
        let on_disk = make_test_png(20, 20);
        std::fs::write(&path, &on_disk).unwrap();

        let text = format!(
            "look at [Image #1: {}] please",
            dunce::canonicalize(&path).unwrap().display(),
        );
        let allowed = [dunce::canonicalize(dir.path()).unwrap()];
        let blocks = build_content_blocks_with_prefixes(text, vec![], Some(&allowed));

        // Text block + 1 recovered image.
        assert_eq!(blocks.len(), 2);
        let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] else {
            panic!("expected recovered Image block");
        };
        assert_eq!(ic.mime_type, "image/png");
        assert!(!ic.data.is_empty());
        assert!(
            ic.uri.as_deref().unwrap().starts_with("file://"),
            "uri should be file:// URI form, got {:?}",
            ic.uri
        );
        // Base64-encoded `data` must round-trip back to the on-disk
        // PNG bytes. A regression emitting raw bytes or
        // double-encoding would fail this assertion.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&ic.data)
            .expect("data must be valid base64");
        assert_eq!(decoded, on_disk);
        // Placeholder anchor stays but the path is now stripped — the
        // image is already attached inline, so the model has no reason
        // to call `Read` on the path (and the path component would
        // tempt it to). The bracketed `[Image #N]` form preserves the
        // positional anchor inside the prose.
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("first block must be text");
        };
        assert!(
            t.text.contains("[Image #1]"),
            "anchor should be preserved on successful recovery, got: {}",
            t.text
        );
        assert!(
            !t.text.contains("[Image #1:"),
            "the path-bearing form must be stripped once the image is attached, got: {}",
            t.text
        );
        assert!(
            !t.text.contains("orphan.png"),
            "file name leaked through after path strip, got: {}",
            t.text
        );
    }

    // Regression: when the user types `[Image #N: <path>]` and the file
    // exists at paste time, the prompt-widget creates a PastedImage and
    // the image is attached inline. The path in the prompt text must
    // then be stripped to `[Image #N]` so the model doesn't follow up
    // with a redundant `Read` tool call on the same file.
    #[test]
    fn build_blocks_strips_path_when_pasted_image_attached() {
        let mut img = make_real_image(40, 30);
        img.display_number = 1;
        img.source_path = Some(std::path::PathBuf::from(
            "/Users/me/Desktop/Screenshot 2026-05-22 at 16.01.21.png",
        ));
        let text = "what is that?[Image #1: /Users/me/Desktop/Screenshot 2026-05-22 at 16.01.21.png] thanks".to_string();

        // Hermetic: no orphan-recovery path needed (PastedImage matches).
        let blocks = build_content_blocks_with_prefixes(text, vec![img], Some(&[]));

        assert_eq!(blocks.len(), 2, "expected text + 1 inline image");
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("first block must be text");
        };
        assert_eq!(
            t.text, "what is that?[Image #1] thanks",
            "placeholder path must be stripped while the anchor survives"
        );
        let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] else {
            panic!("second block must be the inline image");
        };
        assert_eq!(ic.mime_type, "image/png");
        assert!(!ic.data.is_empty(), "inline image must carry base64 bytes");
    }

    #[test]
    fn build_blocks_orphan_placeholder_missing_file_is_stripped_with_warn() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("nope.png");
        let text = format!("before [Image #4: {}] after", bogus.display());

        let allowed = [dunce::canonicalize(dir.path()).unwrap()];
        let blocks = build_content_blocks_with_prefixes(text, vec![], Some(&allowed));
        // No image attached, only text block.
        assert_eq!(blocks.len(), 1);
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("expected text block");
        };
        // Pin the exact post-strip text — the strip seam (space
        // before + space after the placeholder) collapses to a
        // single space.
        assert_eq!(t.text, "before after");
    }

    #[test]
    fn collapse_strip_seam_preserves_code_block_indentation() {
        // A naive implementation could collapse ALL 2+-space runs
        // in the text after a single strip; indented code further
        // down the text must survive intact.
        let mut text = String::from("hello [Image #1: /tmp/x.png] world\n    fn foo() {}\n");
        let span = (text.find("[Image").unwrap(), text.find("]").unwrap() + 1);
        collapse_strip_seam(&mut text, span.0, span.1);
        // The strip seam (space–space) collapses to one space, while
        // the 4-space code indentation further down is untouched.
        assert_eq!(text, "hello world\n    fn foo() {}\n");
    }

    #[test]
    fn collapse_strip_seam_preserves_double_space_elsewhere() {
        // Punctuation double-space far from the strip seam stays.
        let mut text = String::from("a.  b [Image #1: /x.png] c.  d");
        let span = (text.find("[Image").unwrap(), text.find("]").unwrap() + 1);
        collapse_strip_seam(&mut text, span.0, span.1);
        assert_eq!(text, "a.  b c.  d");
    }

    #[test]
    fn build_blocks_orphan_skipped_when_pasted_image_present() {
        // PastedImage with display_number 1 is attached; the matching
        // placeholder must NOT trigger an on-disk load even when the
        // placeholder's path doesn't exist.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.png");
        let img = make_real_image(40, 40);
        let text = format!("see [Image #1: {}]", missing.display());

        let allowed = [dunce::canonicalize(dir.path()).unwrap()];
        let blocks = build_content_blocks_with_prefixes(text, vec![img], Some(&allowed));
        // Text + the PastedImage's own block; no orphan recovery
        // (skipped because `display_number` matches).
        assert_eq!(blocks.len(), 2);
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("first block must be text");
        };
        // Phase 2 universal strip: the anchor `[Image #1]` survives so
        // the model can place the inline image, but the path is gone
        // even though no orphan-recovery loaded it (a PastedImage
        // already provided the bytes). This avoids the "model calls
        // Read on the path even though the image is attached" pattern.
        assert!(
            t.text.contains("[Image #1]"),
            "anchor must survive when a PastedImage backs the placeholder, got: {}",
            t.text
        );
        assert!(
            !t.text.contains("[Image #1:"),
            "path must be stripped even when the load was skipped, got: {}",
            t.text
        );
    }

    #[test]
    fn build_blocks_no_workspace_falls_back_to_legacy_behavior() {
        // Without a workspace cwd, orphan placeholders are not loaded
        // from disk (legacy behaviour preserved). The Phase 2 path
        // strip still runs — it is independent of the allowlist —
        // because the model-facing prompt should never contain the
        // path-bearing form regardless of whether the load happened.
        let text = "look at [Image #2: /nowhere/missing.png]";
        let blocks = build_content_blocks_with_workspace(text.into(), vec![], None);
        // Text block only — no recovery without a workspace.
        assert_eq!(blocks.len(), 1);
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("expected text block");
        };
        assert!(
            t.text.contains("[Image #2]"),
            "anchor must survive the no-workspace path, got: {}",
            t.text
        );
        assert!(
            !t.text.contains("[Image #2:"),
            "path strip is unconditional once the regex matches, got: {}",
            t.text
        );
    }

    // ----- TUI aggregate-cap injectable variant + tests -----------------
    //
    // Mirrors the server-side
    // `recover_orphan_placeholders_with_prefixes_and_caps` tests so a
    // refactor of the TUI loop (e.g. moving `aggregate_bytes += ...`
    // before the cap check, or swapping `break` for `continue`) is
    // caught here even though the cap constant is shared.

    /// Two orphan placeholders, aggregate cap admits exactly one.
    /// Asserts the second placeholder did NOT load (only one image
    /// block in the output) and the first one did.
    #[test]
    fn build_blocks_orphan_aggregate_cap_breaks_loop() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.png");
        let p2 = dir.path().join("b.png");
        let png = make_test_png(20, 20);
        std::fs::write(&p1, &png).unwrap();
        std::fs::write(&p2, &png).unwrap();
        let c1 = dunce::canonicalize(&p1).unwrap();
        let c2 = dunce::canonicalize(&p2).unwrap();
        let text = format!("[Image #1: {}] [Image #2: {}]", c1.display(), c2.display());
        let allowed = [dunce::canonicalize(dir.path()).unwrap()];
        // Cap admits the first image but not the cumulative second.
        let blocks =
            build_content_blocks_with_prefixes_and_caps(text, vec![], Some(&allowed), png.len());
        // Text + 1 recovered image (not 2).
        assert_eq!(blocks.len(), 2);
        let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] else {
            panic!("expected recovered Image block");
        };
        let attached_uri = ic.uri.as_deref().unwrap();
        assert!(
            attached_uri.contains("a.png"),
            "first placeholder must be the one kept, got: {attached_uri}"
        );
        // Cap-breach is a `break` path, not an `Err`-path strip —
        // the rejected placeholder's anchor must survive in the
        // prompt. Symmetric to the single-image inclusive-boundary
        // pin in
        // `build_blocks_orphan_aggregate_cap_inclusive_boundary_rejects_at_one_below`.
        //
        // Phase 2 universal path-strip: the `: <path>` component is
        // stripped uniformly across every surviving placeholder, so
        // the anchor `[Image #2]` is what survives. Intent: anchor
        // preserved so the model still sees the in-prose position;
        // path is gone because the image isn't attached and a bare
        // path would tempt a `Read`.
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("expected text block");
        };
        assert!(
            t.text.contains("[Image #2]"),
            "anchor of rejected placeholder must survive cap breach, got: {}",
            t.text
        );
        assert!(
            !t.text.contains("[Image #2:"),
            "path of rejected placeholder must be stripped, got: {}",
            t.text
        );
    }

    /// Inclusive boundary: cap == single image size admits the image.
    #[test]
    fn build_blocks_orphan_aggregate_cap_inclusive_boundary() {
        use base64::Engine as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.png");
        let png = make_test_png(20, 20);
        std::fs::write(&path, &png).unwrap();
        let canon = dunce::canonicalize(&path).unwrap();
        let text = format!("[Image #1: {}]", canon.display());
        let allowed = [dunce::canonicalize(dir.path()).unwrap()];
        let blocks =
            build_content_blocks_with_prefixes_and_caps(text, vec![], Some(&allowed), png.len());
        assert_eq!(blocks.len(), 2);
        // Symmetric to
        // `build_blocks_orphan_placeholder_loaded_from_disk` —
        // decode the base64 data and assert byte-for-byte equality
        // with the on-disk PNG so a regression emitting wrong bytes
        // at the inclusive boundary is caught.
        let agent_client_protocol::ContentBlock::Image(ic) = &blocks[1] else {
            panic!("expected recovered Image block");
        };
        assert_eq!(ic.mime_type, "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&ic.data)
            .expect("data must be valid base64");
        assert_eq!(decoded, png);
    }

    /// Reject side: cap == image size - 1 rejects the image.
    ///
    /// **Text-side contract.** Aggregate-cap breach is a `break`
    /// path in `resolve_orphan_placeholders`, not a per-image
    /// `Err` path. Only `Err`-path failures strip the placeholder
    /// text; cap-breach intentionally **leaves the placeholder
    /// text intact** because the load itself succeeded (the file
    /// is valid, just doesn't fit in the budget). The test pins
    /// both halves of this contract: no image block AND
    /// placeholder text preserved.
    #[test]
    fn build_blocks_orphan_aggregate_cap_inclusive_boundary_rejects_at_one_below() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.png");
        let png = make_test_png(20, 20);
        std::fs::write(&path, &png).unwrap();
        let canon = dunce::canonicalize(&path).unwrap();
        let text = format!("[Image #1: {}]", canon.display());
        let allowed = [dunce::canonicalize(dir.path()).unwrap()];
        let blocks = build_content_blocks_with_prefixes_and_caps(
            text,
            vec![],
            Some(&allowed),
            png.len() - 1,
        );
        // Cap below image size → no recovered image; only the text
        // block remains.
        assert_eq!(blocks.len(), 1);
        let agent_client_protocol::ContentBlock::Text(t) = &blocks[0] else {
            panic!("expected text block");
        };
        // Placeholder anchor is NOT stripped on aggregate-cap
        // breach (cap-breach is a `break` path, not a load `Err`).
        // Pinning the preservation half of the contract.
        //
        // Phase 2 path-strip update: the bracketed anchor
        // `[Image #N]` survives, but the `: <path>` component is
        // stripped uniformly across every surviving placeholder. The
        // model can still see *where* in the prose the image was
        // referenced via the anchor; the path metadata is no longer
        // leaked because no image is actually attached.
        assert!(
            t.text.contains("[Image #1]"),
            "anchor must survive aggregate-cap breach, got: {}",
            t.text
        );
        assert!(
            !t.text.contains("[Image #1:"),
            "path-bearing form must be stripped, got: {}",
            t.text
        );
    }

    // ----- T8: cleanup and lifecycle edge cases ------------------------------

    #[test]
    fn clear_deletes_staged_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let tmp_path = dir.path().join("staged.png");
        std::fs::write(&tmp_path, b"fake").unwrap();
        assert!(tmp_path.exists());

        let mut images = vec![PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: 4,
            encoded_bytes: None,
            source_path: None,
            staged_temp_path: Some(tmp_path.clone()),
            session_image_path: None, // not yet persisted to session
            preview: PromptImagePreview::default(),
        }];
        let mut counter = 1;
        clear(&mut images, &mut counter);

        assert!(images.is_empty());
        assert!(!tmp_path.exists(), "staged temp file should be deleted");
    }

    #[test]
    fn clear_preserves_session_persisted_file() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("image-abc.png");
        std::fs::write(&session_path, b"real").unwrap();

        let mut images = vec![PastedImage {
            element_id: ElementId::from_raw(1),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: 4,
            encoded_bytes: None,
            source_path: None,
            staged_temp_path: None,
            session_image_path: Some(session_path.clone()),
            preview: PromptImagePreview::default(),
        }];
        let mut counter = 1;
        clear(&mut images, &mut counter);

        assert!(session_path.exists(), "session file should NOT be deleted");
    }

    #[test]
    fn reconcile_deletes_staged_temp_for_removed_chip() {
        let dir = tempfile::tempdir().unwrap();
        let tmp_path = dir.path().join("orphan.png");
        std::fs::write(&tmp_path, b"orphan").unwrap();

        let mut images = vec![PastedImage {
            element_id: ElementId::from_raw(42),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: 6,
            encoded_bytes: None,
            source_path: None,
            staged_temp_path: Some(tmp_path.clone()),
            session_image_path: None,
            preview: PromptImagePreview::default(),
        }];

        // Element 42 is no longer live.
        let live: HashSet<ElementId> = HashSet::new();
        reconcile(&mut images, &live);

        assert!(images.is_empty());
        assert!(!tmp_path.exists(), "staged temp file should be cleaned up");
    }

    #[test]
    fn reconcile_preserves_session_file_for_removed_chip() {
        let dir = tempfile::tempdir().unwrap();
        let session_path = dir.path().join("persisted.png");
        std::fs::write(&session_path, b"keep").unwrap();

        let mut images = vec![PastedImage {
            element_id: ElementId::from_raw(42),
            display_number: 1,
            mime_type: "image/png".into(),
            dimensions: None,
            byte_len: 4,
            encoded_bytes: None,
            source_path: None,
            staged_temp_path: None,
            session_image_path: Some(session_path.clone()),
            preview: PromptImagePreview::default(),
        }];

        let live: HashSet<ElementId> = HashSet::new();
        reconcile(&mut images, &live);

        assert!(images.is_empty());
        assert!(
            session_path.exists(),
            "session-persisted file should remain"
        );
    }

    // ----- ScrollbackImageRef ------------------------------------------------

    #[test]
    fn scrollback_ref_from_valid_image_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        std::fs::write(&path, make_test_png(100, 80)).unwrap();

        let r = ScrollbackImageRef::from_path(&path).unwrap();
        assert_eq!(r.path, path);
    }

    #[test]
    fn scrollback_ref_rejects_non_image_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.txt");
        std::fs::write(&path, b"hello").unwrap();

        assert!(ScrollbackImageRef::from_path(&path).is_none());
    }

    #[test]
    fn scrollback_ref_rejects_undecodable_image_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-image.png");
        std::fs::write(&path, b"hello").unwrap();

        assert!(ScrollbackImageRef::from_path(&path).is_none());
    }

    #[test]
    fn scrollback_ref_accepts_valid_jpeg_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        std::fs::write(&path, make_test_jpeg(25, 20)).unwrap();

        let r = ScrollbackImageRef::from_path(&path).unwrap();
        assert_eq!(r.path, path);
    }

    #[test]
    fn extract_skips_undecodable_image_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-image.png");
        std::fs::write(&path, b"hello").unwrap();

        let text = format!("![bad]({})", path.display());
        let refs = extract_image_refs(&text);
        assert!(refs.is_empty());
    }

    #[test]
    fn scrollback_ref_rejects_missing_file() {
        assert!(ScrollbackImageRef::from_path("/nonexistent/image.png").is_none());
    }

    // ----- extract_image_refs ------------------------------------------------

    #[test]
    fn extract_markdown_image_ref() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hero.png");
        std::fs::write(&path, make_test_png(50, 50)).unwrap();

        let text = format!("Here is the image: ![hero]({})", path.display());
        let refs = extract_image_refs(&text);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, path);
    }

    #[test]
    fn extract_bare_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        std::fs::write(&path, make_test_png(25, 20)).unwrap();

        let text = format!("saved to {} (1234 bytes)", path.display());
        let refs = extract_image_refs(&text);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, path);
    }

    #[test]
    fn extract_bare_absolute_jpeg_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        std::fs::write(&path, make_test_jpeg(25, 20)).unwrap();

        let text = format!("saved to {} (1234 bytes)", path.display());
        let refs = extract_image_refs(&text);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, path);
    }

    #[test]
    fn media_only_markdown_accepts_image_ref_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("generated.jpg");
        std::fs::write(&path, make_test_jpeg(25, 20)).unwrap();

        let text = format!(" \n![generated]({})\n", path.display());
        let refs = extract_image_refs(&text);
        assert_eq!(refs.len(), 1);
        assert!(is_media_only_markdown(&text, refs.len()));
    }

    #[test]
    fn media_only_markdown_rejects_mixed_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("generated.jpg");
        std::fs::write(&path, make_test_jpeg(25, 20)).unwrap();

        let text = format!("Here is an image: ![generated]({})", path.display());
        let refs = extract_image_refs(&text);
        assert_eq!(refs.len(), 1);
        assert!(!is_media_only_markdown(&text, refs.len()));
    }

    #[test]
    fn extract_deduplicates_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.png");
        std::fs::write(&path, make_test_png(10, 10)).unwrap();

        let text = format!("![a]({p}) and ![b]({p})", p = path.display());
        let refs = extract_image_refs(&text);
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn extract_skips_nonexistent_paths() {
        let text = "![img](/tmp/nonexistent_abc123.png)";
        let refs = extract_image_refs(text);
        assert!(refs.is_empty());
    }

    // ----- open_from_path ----------------------------------------------------

    #[test]
    fn open_from_path_valid_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewer.png");
        let png = make_test_png(200, 150);
        std::fs::write(&path, &png).unwrap();

        let viewer = ImageViewerState::open_from_path(&path).unwrap();
        assert_eq!(viewer.image_width, 200);
        assert_eq!(viewer.image_height, 150);
        assert_eq!(viewer.display_number, 1);
        assert_eq!(viewer.image_bytes, png);
        assert_eq!(viewer.display_bytes, viewer.image_bytes);
    }

    #[test]
    fn open_from_path_valid_jpeg_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("viewer.jpg");
        let jpeg = make_test_jpeg(200, 150);
        std::fs::write(&path, &jpeg).unwrap();

        let viewer = ImageViewerState::open_from_path(&path).unwrap();
        assert_eq!(viewer.image_width, 200);
        assert_eq!(viewer.image_height, 150);
        assert_eq!(viewer.image_bytes, jpeg);
    }

    #[test]
    fn open_from_path_missing_file() {
        assert!(
            ImageViewerState::open_from_path(std::path::Path::new("/no/such/file.png")).is_none()
        );
    }

    // ----- open_from_path_deferred -------------------------------------------

    #[test]
    fn deferred_open_starts_in_loading_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deferred.png");
        let png = make_test_png(80, 60);
        std::fs::write(&path, &png).unwrap();

        let viewer = ImageViewerState::open_from_path_deferred(&path);
        assert!(viewer.loading);
        assert!(viewer.image_bytes.is_empty());
        assert_eq!(viewer.image_width, 0);
        assert_eq!(viewer.title.as_deref(), Some("deferred.png"));
    }

    #[test]
    fn deferred_finish_loading_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deferred.png");
        let png = make_test_png(80, 60);
        std::fs::write(&path, &png).unwrap();

        let mut viewer = ImageViewerState::open_from_path_deferred(&path);
        assert!(viewer.finish_loading());
        assert!(!viewer.loading);
        assert_eq!(viewer.image_width, 80);
        assert_eq!(viewer.image_height, 60);
        assert_eq!(viewer.image_bytes, png);
    }

    #[test]
    fn deferred_finish_loading_missing_file_fails() {
        let mut viewer =
            ImageViewerState::open_from_path_deferred(std::path::Path::new("/no/such/file.png"));
        assert!(!viewer.finish_loading());
    }

    #[test]
    fn deferred_finish_loading_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idem.png");
        std::fs::write(&path, make_test_png(10, 10)).unwrap();

        let mut viewer = ImageViewerState::open_from_path_deferred(&path);
        assert!(viewer.finish_loading());
        // Second call is a no-op, returns true.
        assert!(viewer.finish_loading());
        assert!(!viewer.loading);
    }
}
