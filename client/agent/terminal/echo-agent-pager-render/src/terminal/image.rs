//! Terminal inline image rendering (Kitty / iTerm2 protocols).
//!
//! Provides escape-sequence helpers for rendering images inside the
//! existing preview overlay. The text-fallback path in
//! [`crate::render::image_overlay`] remains the primary preview; this
//! module adds pixel-level rendering for supported terminals.
//!
//! # Supported protocols
//!
//! - **Kitty graphics protocol**: used by Kitty, Ghostty, WezTerm, Warp
//! - **iTerm2 inline images**: helpers exist but are currently gated off in
//!   [`protocol_for_brand()`] (see there for why); the text fallback is used
//!   for iTerm2 instead.
//!
//! # Usage
//!
//! 1. Call [`detect_graphics_protocol()`] once (cached).
//! 2. During draw, if an image preview is active, call
//!    [`render_kitty_image()`] or [`render_iterm2_image()`] to build the
//!    escape sequence.
//! 3. Write the escape sequence to stderr **after** the ratatui cell
//!    flush but inside the synchronized-output block.
//! 4. Coordinate shared ID-1 ownership through [`super::overlay`].

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{TerminalName, terminal_context};

// -------------------------------------------------------------------------
// Graphics protocol detection
// -------------------------------------------------------------------------

/// Graphics protocol supported by the current terminal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GraphicsProtocol {
    /// Kitty graphics protocol (also used by Ghostty, WezTerm).
    Kitty,
    /// iTerm2 inline images protocol.
    ITerm2,
    /// No graphics protocol available — text fallback only.
    #[default]
    None,
}

impl GraphicsProtocol {
    /// Whether this protocol can render pixel images inline.
    pub fn supports_images(self) -> bool {
        !matches!(self, Self::None)
    }
}

static GRAPHICS_PROTOCOL: OnceLock<GraphicsProtocol> = OnceLock::new();

/// When set, scrollback inline-media overlays are forced **off** process-wide,
/// regardless of the terminal's graphics capability. The scrollback-native
/// minimal mode (`echoagent --minimal`) sets this once at startup: it never runs the
/// interactive draw loop that paints inline images, so committed media blocks
/// must always fall back to the `[Open …]` text affordance — and must not
/// reserve blank image rows. See [`set_inline_overlay_force_off`].
static INLINE_OVERLAY_FORCE_OFF: AtomicBool = AtomicBool::new(false);

/// Force scrollback inline-media overlays off (`off = true`) or restore the
/// capability-based default (`off = false`) process-wide. Called once at
/// startup by the pager when minimal mode is active.
pub fn set_inline_overlay_force_off(off: bool) {
    INLINE_OVERLAY_FORCE_OFF.store(off, Ordering::Relaxed);
}

/// Whether scrollback inline-media overlays are currently forced off — i.e. the
/// process is in minimal/scrollback-native mode, which commits static text and
/// never runs the interactive draw loop. Also used to suppress draw-loop-painted
/// affordances (e.g. the mermaid button row) that would otherwise commit blank.
pub fn scrollback_inline_overlay_forced_off() -> bool {
    INLINE_OVERLAY_FORCE_OFF.load(Ordering::Relaxed)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Per-test override so tests don't depend on the host terminal or the
    /// process-wide `GRAPHICS_PROTOCOL` cache.
    static TEST_PROTOCOL_OVERRIDE: std::cell::Cell<Option<GraphicsProtocol>> =
        const { std::cell::Cell::new(None) };
}

/// Detect and cache the graphics protocol for the current terminal.
pub fn detect_graphics_protocol() -> GraphicsProtocol {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(p) = TEST_PROTOCOL_OVERRIDE.with(|c| c.get()) {
        return p;
    }
    *GRAPHICS_PROTOCOL.get_or_init(|| {
        let ctx = terminal_context();
        if ctx.graphics_protocol_skip_reason().is_some() {
            return GraphicsProtocol::None;
        }
        protocol_for_brand(ctx.brand, cfg!(target_os = "windows"))
    })
}

/// Whether the current terminal can safely host scrollback inline-media
/// overlays.
///
/// This is narrower than "supports Kitty graphics": scrollback media uses
/// Kitty image ids, placement ids, z-index, clearing, and source cropping so
/// images scroll with the text grid. Warp accepts some Kitty image escapes but
/// does not reliably support that placement/scrollback model, which leaves
/// stale or corrupted pixels while scrolling.
pub fn scrollback_inline_overlay_active() -> bool {
    // Minimal mode forces this off process-wide: it never paints inline images,
    // so media must always use the text affordance.
    if INLINE_OVERLAY_FORCE_OFF.load(Ordering::Relaxed) {
        return false;
    }
    let protocol = detect_graphics_protocol();
    if test_protocol_override_active() {
        return protocol == GraphicsProtocol::Kitty;
    }
    scrollback_inline_overlay_active_for_brand(protocol, terminal_context().brand)
}

#[cfg(any(test, feature = "test-support"))]
fn test_protocol_override_active() -> bool {
    TEST_PROTOCOL_OVERRIDE.with(|c| c.get().is_some())
}

#[cfg(not(any(test, feature = "test-support")))]
fn test_protocol_override_active() -> bool {
    false
}

/// Pure capability helper for scrollback inline-media overlays.
fn scrollback_inline_overlay_active_for_brand(
    protocol: GraphicsProtocol,
    brand: TerminalName,
) -> bool {
    matches!(
        (protocol, brand),
        (
            GraphicsProtocol::Kitty,
            TerminalName::Kitty | TerminalName::Ghostty | TerminalName::WezTerm
        )
    )
}

/// Set a per-thread protocol override for tests. Returns a guard that
/// clears it on drop.
#[cfg(any(test, feature = "test-support"))]
pub fn set_protocol_for_test(p: GraphicsProtocol) -> TestProtocolGuard {
    TEST_PROTOCOL_OVERRIDE.with(|c| c.set(Some(p)));
    TestProtocolGuard
}

/// RAII guard that clears the test protocol override on drop.
#[cfg(any(test, feature = "test-support"))]
pub struct TestProtocolGuard;

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestProtocolGuard {
    fn drop(&mut self) {
        TEST_PROTOCOL_OVERRIDE.with(|c| c.set(None));
    }
}

/// Map terminal brand to graphics protocol. Returns `None` on Windows
/// because ConPTY strips the Kitty/iTerm2 APC escape sequences before
/// they reach the host terminal.
///
/// Parameterised by `is_windows` so unit tests can exercise both paths
/// on any OS.
pub fn protocol_for_brand(brand: TerminalName, is_windows: bool) -> GraphicsProtocol {
    if is_windows {
        return GraphicsProtocol::None;
    }
    match brand {
        TerminalName::Kitty => GraphicsProtocol::Kitty,
        TerminalName::Ghostty => GraphicsProtocol::Kitty,
        TerminalName::WezTerm => GraphicsProtocol::Kitty,
        TerminalName::WarpTerminal => GraphicsProtocol::Kitty,
        // iTerm2's OSC 1337 inline-image protocol lacks the image-id, z-index,
        // source-crop, and clear primitives the Kitty protocol has, so overlay
        // images don't track the text grid — they paint wrong or never appear
        // (leaving a stuck "Loading…" hint). Disable it; the text/metadata
        // fallback is used instead. Re-enable with `ITerm2` once verified.
        TerminalName::Iterm2 => GraphicsProtocol::None,
        _ => GraphicsProtocol::None,
    }
}

// -------------------------------------------------------------------------
// Kitty graphics protocol
// -------------------------------------------------------------------------

/// Shared placement ID; every renderer must coordinate through [`super::overlay`].
pub(super) const KITTY_PLACEMENT_ID: u32 = 1;

/// Kitty graphics protocol image format identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KittyImageFormat {
    /// PNG image data (`f=100`).
    Png,
}

impl KittyImageFormat {
    fn code(self) -> u16 {
        match self {
            Self::Png => 100,
        }
    }
}

/// Detect the Kitty graphics format code for encoded image bytes.
pub fn kitty_format_from_bytes(image_data: &[u8]) -> Option<KittyImageFormat> {
    match echo_agent_shared::clipboard::mime_from_bytes(image_data) {
        "image/png" => Some(KittyImageFormat::Png),
        _ => None,
    }
}

/// Whether Kitty can directly render this encoded MIME type in raw-byte mode.
pub fn kitty_mime_is_directly_supported(mime_type: &str) -> bool {
    mime_type == "image/png"
}

/// Prepare encoded image bytes for Kitty's raw-byte overlay path.
///
/// Kitty accepts encoded PNG bytes via `f=100`, but not encoded JPEG/WebP/etc.
/// Convert other decodable images to PNG before handing them to the centered
/// overlay renderer. Callers must keep this out of draw paths.
///
/// On macOS, uses `sips` (Apple CoreGraphics) which handles ICC colour
/// profiles correctly. Falls back to the `image` crate on other platforms.
pub fn prepare_kitty_overlay_image_bytes(image_data: &[u8]) -> Option<Vec<u8>> {
    if kitty_format_from_bytes(image_data).is_some() {
        return Some(image_data.to_vec());
    }

    // On macOS, convert via `sips` through a temp file. CoreGraphics
    // handles ICC colour profiles correctly, avoiding the artifacts
    // that the `image` crate's JPEG→PNG path can produce.
    if cfg!(target_os = "macos")
        && let Some(png) = convert_via_sips(image_data)
    {
        return Some(png);
    }

    let img = image::ImageReader::new(std::io::Cursor::new(image_data))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;

    let mut png = Vec::new();
    {
        use image::ExtendedColorType;
        use image::ImageEncoder;
        use image::codecs::png::{CompressionType, FilterType, PngEncoder};

        let rgba = img.to_rgba8();
        let encoder =
            PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Adaptive);
        encoder
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ExtendedColorType::Rgba8,
            )
            .ok()?;
    }
    Some(png)
}

/// Convert image bytes to PNG via macOS `sips` using temp files.
fn convert_via_sips(image_data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;

    let tmp_dir = std::env::temp_dir();
    let id = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let src = tmp_dir.join(format!("echo-agent-sips-{id}-{ts}.dat"));
    let dst = tmp_dir.join(format!("echo-agent-sips-{id}-{ts}.png"));

    // Write source bytes to temp file.
    let mut f = std::fs::File::create(&src).ok()?;
    f.write_all(image_data).ok()?;
    f.sync_all().ok()?;
    drop(f);

    let mut sips_cmd = std::process::Command::new("sips");
    sips_cmd
        .args(["-s", "format", "png"])
        .arg(&src)
        .arg("--out")
        .arg(&dst)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    echo_agent_tty_utils::detach_std_command(&mut sips_cmd);
    let status = sips_cmd.status().ok()?;

    let _ = std::fs::remove_file(&src);

    if !status.success() || !dst.is_file() {
        let _ = std::fs::remove_file(&dst);
        return None;
    }

    let png = std::fs::read(&dst).ok()?;
    let _ = std::fs::remove_file(&dst);
    Some(png)
}

/// Prepare encoded image bytes for the currently detected overlay protocol.
pub fn prepare_overlay_image_bytes(image_data: &[u8]) -> Option<Vec<u8>> {
    match detect_graphics_protocol() {
        GraphicsProtocol::Kitty => prepare_kitty_overlay_image_bytes(image_data),
        GraphicsProtocol::ITerm2 => Some(image_data.to_vec()),
        GraphicsProtocol::None => None,
    }
}

/// Build a Kitty graphics protocol escape sequence to display encoded image data.
///
/// The image is transmitted inline as base64-encoded data and scaled by the
/// terminal to fit `cols` columns × `rows` rows. The terminal handles
/// HiDPI/Retina scaling correctly since it knows the actual cell pixel
/// dimensions.
///
/// Uses `a=T` (transmit + display), `f=<format>` (PNG format), `t=d`
/// (direct data transmission), `q=2` (suppress responses), `C=1` (preserve
/// cursor position), and `z=1` (draw above text cells), chunked into 4096-byte
/// pieces.
pub fn render_kitty_image(
    image_data: &[u8],
    format: KittyImageFormat,
    cols: u16,
    rows: u16,
) -> String {
    render_kitty_image_z(image_data, format, cols, rows, 1)
}

/// Render a Kitty image with a specific z-index.
///
/// `z=1`: above text (modal overlays). `z=-1`: below text, above
/// background (inline scrollback media — dropdowns render on top).
pub fn render_kitty_image_z(
    image_data: &[u8],
    format: KittyImageFormat,
    cols: u16,
    rows: u16,
    z: i32,
) -> String {
    let header = format!(
        "a=T,f={},t=d,q=2,C=1,z={},i={},p={},c={},r={}",
        format.code(),
        z,
        KITTY_PLACEMENT_ID,
        KITTY_PLACEMENT_ID,
        cols,
        rows,
    );
    kitty_chunked_escape(image_data, &header)
}

/// Transmit image data to the terminal without displaying it (`a=t`).
/// Use `place_kitty_image` to display it at a position.
pub fn transmit_kitty_image(image_data: &[u8], format: KittyImageFormat, image_id: u32) -> String {
    let header = format!("a=t,f={},t=d,q=2,i={}", format.code(), image_id);
    kitty_chunked_escape(image_data, &header)
}

/// Encode image data as chunked Kitty escape sequences.
/// `first_chunk_header` is the metadata for the first chunk (action, format, etc.).
fn kitty_chunked_escape(image_data: &[u8], first_chunk_header: &str) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(image_data);

    let chunk_size = 4096;
    let chunks: Vec<&str> = b64
        .as_bytes()
        .chunks(chunk_size)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();

    let mut out = String::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let is_last = i == chunks.len() - 1;
        let m = if is_last { 0 } else { 1 };
        if i == 0 {
            out.push_str(&format!("\x1b_G{first_chunk_header},m={m};{chunk}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gq=2,m={m};{chunk}\x1b\\"));
        }
    }
    out
}

/// Place an already-transmitted image at the cursor position (`a=p`).
///
/// Tiny escape (~50 bytes) — no image data, just placement metadata.
pub fn place_kitty_image(image_id: u32, cols: u16, rows: u16, z: i32) -> String {
    format!(
        "\x1b_Ga=p,i={},p={},c={},r={},z={},C=1,q=2\x1b\\",
        image_id, image_id, cols, rows, z,
    )
}

/// Place an already-transmitted image with source cropping (`a=p`).
///
/// `src_x, src_y, src_w, src_h`: pixel region of the source image to display.
#[allow(clippy::too_many_arguments)]
pub fn place_kitty_image_cropped(
    image_id: u32,
    cols: u16,
    rows: u16,
    z: i32,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
) -> String {
    format!(
        "\x1b_Ga=p,i={},p={},c={},r={},z={},x={},y={},w={},h={},C=1,q=2\x1b\\",
        image_id, image_id, cols, rows, z, src_x, src_y, src_w, src_h,
    )
}

/// Build a Kitty escape sequence to delete a specific image by ID.
pub fn clear_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=i,i={},q=2\x1b\\", image_id)
}

// -------------------------------------------------------------------------
// iTerm2 inline images protocol
// -------------------------------------------------------------------------

/// Build an iTerm2 inline image escape sequence.
///
/// Uses `\x1b]1337;File=inline=1;width=Ncells;height=Ncells;preserveAspectRatio=1:BASE64\x07`.
pub fn render_iterm2_image(image_data: &[u8], cols: u16, rows: u16) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(image_data);
    format!(
        "\x1b]1337;File=inline=1;width={cols}cells;height={rows}cells;preserveAspectRatio=1:{b64}\x07",
        cols = cols,
        rows = rows,
        b64 = b64,
    )
}

// -------------------------------------------------------------------------
// Shared overlay helpers
// -------------------------------------------------------------------------

/// Build the full escape-sequence string to render image data at a cell
/// position using the provided graphics protocol.
///
/// For Kitty: transmits image data once (`a=t`) then places it (`a=p`). Pass
/// `retransmit = false` on subsequent frames to emit only the placement escape
/// (~50 bytes) instead of re-uploading the full image every redraw.
///
/// For iTerm2: always emits the full inline image escape (no separate transmit
/// primitive). Callers should pass `retransmit = false` after the first frame
/// to avoid re-decoding the same image every tick.
///
/// Returns `None` when no graphics protocol is available.
///
/// Pass `retransmit = false` on subsequent frames to skip the data upload
/// (Kitty: place-only; iTerm2: no-op).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_overlay_image_escapes_for_protocol(
    protocol: GraphicsProtocol,
    image_data: &[u8],
    cols: u16,
    rows: u16,
    cell_x: u16,
    cell_y: u16,
    retransmit: bool,
) -> Option<String> {
    if protocol == GraphicsProtocol::None {
        return None;
    }

    let mut esc = String::new();
    // ANSI cursor positioning is 1-based.
    esc.push_str(&format!("\x1b[{};{}H", cell_y + 1, cell_x + 1));
    match protocol {
        GraphicsProtocol::Kitty => {
            if retransmit {
                // Transmit once, then place — never use a=T (transmit+display)
                // on every frame; that re-uploads the full image at ~10fps and
                // balloons native GPU surface counts in long-lived sessions.
                // Place-only frames do not need image bytes / format detection.
                let format = kitty_format_from_bytes(image_data)?;
                esc.push_str(&transmit_kitty_image(
                    image_data,
                    format,
                    KITTY_PLACEMENT_ID,
                ));
            }
            esc.push_str(&place_kitty_image(
                KITTY_PLACEMENT_ID,
                cols,
                rows,
                1, // above text (modal overlays)
            ));
        }
        GraphicsProtocol::ITerm2 => {
            if retransmit {
                esc.push_str(&render_iterm2_image(image_data, cols, rows));
            }
        }
        GraphicsProtocol::None => unreachable!(),
    }
    Some(esc)
}

/// Transmit inline image data to the terminal GPU.
///
/// Kitty: uploads with the given `image_id`. iTerm2: no-op (data sent per-place).
pub fn transmit_inline_image(image_data: &[u8], image_id: u32) -> Option<String> {
    match detect_graphics_protocol() {
        GraphicsProtocol::Kitty => {
            let format = kitty_format_from_bytes(image_data)?;
            Some(transmit_kitty_image(image_data, format, image_id))
        }
        GraphicsProtocol::ITerm2 => Some(String::new()),
        GraphicsProtocol::None => None,
    }
}

/// Place an inline image at a position, optionally cropping.
///
/// For Kitty: ~80 bytes (no image data, just placement with crop).
/// For iTerm2: sends full image data only when `emit_iterm_data` is true
/// (no crop support). Pass `false` after the first placement to avoid
/// re-decoding the same image on every TUI frame.
#[allow(clippy::too_many_arguments)]
pub fn place_inline_image(
    image_data: &[u8],
    img_w: u32,
    img_h: u32,
    area: ratatui::layout::Rect,
    full_rows: u16,
    top_crop_rows: u16,
    image_id: u32,
    emit_iterm_data: bool,
) -> Option<String> {
    let protocol = detect_graphics_protocol();
    if protocol == GraphicsProtocol::None {
        return None;
    }

    // Compute fit dimensions as if the full image were visible.
    let (fit_cols, fit_rows) = fit_image_to_cells(img_w, img_h, area.width, full_rows);
    let pad_x = area.width.saturating_sub(fit_cols) / 2;
    let img_x = area.x + pad_x;
    let img_y = area.y;

    let mut esc = String::new();
    esc.push_str(&format!("\x1b[{};{}H", img_y + 1, img_x + 1));
    match protocol {
        GraphicsProtocol::Kitty => {
            let visible_rows = area.height.min(fit_rows);
            if top_crop_rows > 0 || visible_rows < fit_rows {
                let src_y = if fit_rows > 0 {
                    (top_crop_rows as u32 * img_h) / fit_rows as u32
                } else {
                    0
                };
                let src_h = if fit_rows > 0 {
                    (visible_rows as u32 * img_h) / fit_rows as u32
                } else {
                    img_h
                };
                esc.push_str(&place_kitty_image_cropped(
                    image_id,
                    fit_cols,
                    visible_rows,
                    -1,
                    0,
                    src_y,
                    img_w,
                    src_h.max(1),
                ));
            } else {
                esc.push_str(&place_kitty_image(image_id, fit_cols, fit_rows, -1));
            }
        }
        GraphicsProtocol::ITerm2 => {
            if emit_iterm_data {
                esc.push_str(&render_iterm2_image(image_data, fit_cols, area.height));
            }
        }
        GraphicsProtocol::None => unreachable!(),
    }
    Some(esc)
}

/// Compute the cell dimensions (`cols`, `rows`) to display an image at
/// its correct aspect ratio within a bounding box of `max_cols × max_rows`.
///
/// Terminal cells are not square — they're roughly twice as tall as wide
/// (typical monospace cell ~8px wide × ~16px tall, ratio ≈ 0.5). This
/// function accounts for that so a 1:1 image appears visually square
/// and a 16:9 screenshot looks like a 16:9 rectangle.
pub fn fit_image_to_cells(img_w: u32, img_h: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if img_w == 0 || img_h == 0 || max_cols == 0 || max_rows == 0 {
        return (max_cols.max(1), max_rows.max(1));
    }

    // Cell aspect ratio: width / height. Typical monospace cell is ~0.5
    // (half as wide as tall). This converts between pixel-space and
    // cell-space so the image doesn't appear stretched.
    let cell_aspect: f64 = 0.5;

    // Image aspect ratio in pixel space.
    let img_aspect = img_w as f64 / img_h as f64;

    // Convert image aspect to cell-space: how many columns per row the
    // image needs to look correct. A cell is `cell_aspect` times as wide
    // as it is tall, so we divide by cell_aspect.
    //   cols_per_row = img_aspect / cell_aspect
    let cols_per_row = img_aspect / cell_aspect;

    // Try fitting by width first.
    let cols_by_width = max_cols;
    let rows_by_width = (cols_by_width as f64 / cols_per_row).round() as u16;

    // Try fitting by height.
    let rows_by_height = max_rows;
    let cols_by_height = (rows_by_height as f64 * cols_per_row).round() as u16;

    // Pick whichever fit stays within bounds.
    if rows_by_width <= max_rows {
        (cols_by_width, rows_by_width.max(1))
    } else {
        (cols_by_height.min(max_cols).max(1), rows_by_height)
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests;
