//! Cross-platform system clipboard access.
//!
//! On macOS, delegates to `pbcopy`/`pbpaste` subprocesses instead of linking
//! AppKit. This is intentional: the `arboard` crate pulls in `objc2-app-kit`,
//! which causes `dyld` to load `AppKit.framework` at process startup. AppKit
//! unconditionally initialises the Metal/IOAccelerator GPU subsystem, allocating
//! ~2 GB of GPU buffer memory even in headless processes (such as the leader).
//!
//! Using `pbcopy`/`pbpaste` avoids the AppKit link entirely, keeping the leader
//! process free of GPU memory overhead while still providing clipboard support
//! for the TUI.
//!
//! The fast pasteboard probe ([`clipboard_image_snapshot`]) keeps that rule: it
//! reaches `NSPasteboard` via objc2 runtime messaging against a lazily
//! `dlopen`ed AppKit, so no binary links AppKit and processes that never probe
//! never load it. The focus/tick probes only message metadata selectors
//! (`changeCount` / `types`) — never content reads — so they stay
//! sub-millisecond and out of macOS 15.4+ pasteboard privacy alerts.
//!
//! Paste-time image reads ([`get_image`] / [`get_attachments`]) use the same
//! lazily loaded AppKit to read raster bytes in-process (`dataForType:`) on
//! the unambiguous hot path (raster advertised, no file-URL type alongside),
//! skipping the ~0.5–0.9 s `osascript` + temp-file round trip.
//! Content is only ever read at an explicit user paste — the same user-intent
//! boundary where the `osascript`/`pbpaste` subprocesses read the pasteboard —
//! and every other pasteboard shape (file URLs, text→furl coercions, AppKit
//! unavailable, read failure) falls back to the unchanged subprocess path.
//! `ECHO_AGENT_CLIPBOARD_NO_NATIVE_READ=1` disables the in-process read entirely
//! (kill switch if a future macOS gates `dataForType:` behind a prompt).
//!
//! On Linux and Windows, `arboard` is used directly (it does not link AppKit on
//! those platforms).
//!
//! ## OSC 52 (remote clipboard)
//!
//! When running over SSH or inside tmux/screen on a remote host, the native
//! system clipboard (`pbcopy`, `arboard`) writes to the *remote* machine's
//! clipboard, which is invisible to the user. OSC 52 is a terminal escape
//! sequence that tells the user's *local* terminal emulator to set its
//! clipboard, tunnelling through SSH and multiplexers.
//!
//! [`set_text_osc52`] writes an OSC 52 sequence to stderr (the TUI output
//! stream). [`is_remote_session`] detects SSH/tmux/screen environments where
//! OSC 52 should be preferred.

/// Encoded image data read from the system clipboard.
///
/// `data` contains encoded image bytes (PNG, JPEG, or TIFF), not raw RGBA
/// pixels. This is suitable for direct persistence and later base64 transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Encoded image bytes (PNG, JPEG, TIFF, etc.).
    pub data: Vec<u8>,
    /// MIME type of the encoded data (e.g. `"image/png"`).
    pub mime_type: String,
}

/// Read text from the system clipboard.
///
/// Returns `Ok(None)` when the clipboard is empty or does not contain text.
pub fn get_text() -> anyhow::Result<Option<String>> {
    platform::get_text()
}

/// Read UTF-8 text from the X11 PRIMARY selection.
///
/// Requires a non-empty `DISPLAY`. Pure X11 may fall back to arboard; XWayland
/// requires xclip or xsel so arboard cannot return Wayland PRIMARY by mistake.
#[cfg(target_os = "linux")]
pub fn get_primary_text() -> anyhow::Result<Option<String>> {
    platform::get_primary_text()
}

/// Whether this Linux process has a non-empty `DISPLAY` environment value.
#[cfg(target_os = "linux")]
pub fn x11_display_env_present() -> bool {
    platform::x11_display_env_present()
}

/// Read an image from the system clipboard.
///
/// Returns `Ok(None)` when the clipboard does not contain an image.
/// The returned [`ImageData`] contains encoded image bytes (not raw RGBA).
pub fn get_image() -> anyhow::Result<Option<ImageData>> {
    platform::get_image()
}

/// Read file references the file manager places on the clipboard
/// when a file is selected but no plain text accompanies it. Backed
/// by `«class furl»` (osascript) on macOS, `arboard::Get::file_list`
/// (CF_HDROP / `text/uri-list`) elsewhere. Returns newline-joined
/// absolute paths in the format the drop-path parser accepts.
pub fn get_file_urls() -> anyhow::Result<Option<String>> {
    platform::get_file_urls()
}

/// File URLs and/or image data from the system clipboard in one probe.
///
/// On macOS empty-pasteboard attachment routing uses a single `osascript`
/// that tries `«class furl»` first, then PNGf → TIFF → JPEG only when no
/// file URLs are present. On other platforms this composes [`get_file_urls`]
/// and [`get_image`].
#[derive(Debug, Clone, Default)]
pub struct ClipboardAttachments {
    /// Newline-joined POSIX paths (same format as [`get_file_urls`]).
    pub file_urls: Option<String>,
    /// Encoded image bytes when the pasteboard holds a raster image.
    pub image: Option<ImageData>,
}

/// Probe file URLs then image (macOS: one `osascript`; elsewhere: two reads).
pub fn get_attachments() -> anyhow::Result<ClipboardAttachments> {
    platform::get_attachments()
}

/// One pasteboard snapshot: `(change_count, has_pasteable_image)` read in a
/// single native pass so both describe the *same* pasteboard state (a copy
/// landing mid-probe can't mix a changeCount from one state with a
/// classification from another).
///
/// `change_count` is the monotonic `NSPasteboard.changeCount` (`None` off-macOS
/// or when AppKit can't load). `has_pasteable_image` is true when a raster type
/// (`public.png` / `public.tiff` / `public.jpeg`) is advertised with no file-URL
/// type alongside (see [`image_pasteable_from_types`]); `false` off-macOS.
///
/// macOS native and sub-millisecond: inspects metadata only (no data read, no
/// subprocess), so it is safe for the focus-driven UI path where [`get_image`]'s
/// `osascript` round-trip (~0.9 s) would be unacceptable.
pub fn clipboard_image_snapshot() -> (Option<u64>, bool) {
    platform::clipboard_image_snapshot()
}

/// Cheap pasteboard `changeCount` read: a SINGLE native message, no type scan
/// and no data read. This is the changeCount-first hot path of the focus-driven
/// clipboard-image tip — each throttled poll calls only this, and pays for the
/// heavier [`clipboard_image_snapshot`] classification ONLY when the count
/// changed since the last look. `None` off-macOS or when AppKit can't load.
pub fn clipboard_change_count() -> Option<u64> {
    platform::clipboard_change_count()
}

/// Whether the fast image probe exists on this platform. Gates the focus-driven
/// clipboard-image tip so non-macOS never probes.
pub fn clipboard_image_probe_supported() -> bool {
    cfg!(target_os = "macos")
}

/// Trigger the one-time lazy AppKit `dlopen` — the expensive part of the macOS
/// probe (it loads the framework and its GPU init) — WITHOUT reading the
/// pasteboard, so a later synchronous [`clipboard_image_snapshot`] is just the
/// cheap metadata read. The load is memoised, and since this touches no
/// pasteboard it is sound to call from a background thread. No-op off-macOS.
pub fn clipboard_prewarm() {
    platform::clipboard_prewarm();
}

/// Decide "pasteable image on the board" from the advertised pasteboard type
/// identifiers (the macOS fast probe's classification rule).
///
/// File-manager copies (Finder ⌘C on a file) advertise a file-icon raster
/// ALONGSIDE file URLs, and paste routes those through path handling —
/// [`get_attachments`]'s probe tries `«class furl»` first and reads image data
/// only when no file URLs are present — so any file-URL advertisement
/// (`public.file-url`, or its pre-UTI spelling `NSFilenamesPboardType`) means
/// the raster is NOT what ctrl+v inserts. Kept platform-independent so the
/// routing rule is unit-tested on every platform.
#[cfg(any(target_os = "macos", test))]
fn image_pasteable_from_types<'a>(types: impl IntoIterator<Item = &'a [u8]>) -> bool {
    let mut has_image = false;
    for ty in types {
        if matches!(ty, b"public.file-url" | b"NSFilenamesPboardType") {
            return false;
        }
        if matches!(ty, b"public.png" | b"public.tiff" | b"public.jpeg") {
            has_image = true;
        }
    }
    has_image
}

/// Raster pasteboard UTIs in read-priority order with their MIME types.
///
/// Mirrors the `osascript` probes' coercion order (`PNGf` → `TIFF` → `JPEG`)
/// so the native in-process read yields the same class the subprocess path
/// would have picked for the same pasteboard.
#[cfg(any(target_os = "macos", test))]
const NATIVE_IMAGE_TYPES: &[(&[u8], &str)] = &[
    (b"public.png", "image/png"),
    (b"public.tiff", "image/tiff"),
    (b"public.jpeg", "image/jpeg"),
];

/// Pick which advertised raster type the native read should request.
///
/// `None` unless the advertised type list classifies as a pasteable image
/// under [`image_pasteable_from_types`] (raster present, no file-URL type
/// alongside — file URLs win and route through the `osascript` furl path).
/// Pure so the routing rule is unit-tested on every platform.
#[cfg(any(target_os = "macos", test))]
fn native_image_type_from_types(
    types: &[impl AsRef<[u8]>],
) -> Option<(&'static [u8], &'static str)> {
    if !image_pasteable_from_types(types.iter().map(|t| t.as_ref())) {
        return None;
    }
    for (uti, mime) in NATIVE_IMAGE_TYPES {
        if types.iter().any(|t| t.as_ref() == *uti) {
            return Some((uti, mime));
        }
    }
    None
}

/// Map an image MIME type to a file extension.
///
/// Returns `"bin"` for unrecognized types.
pub fn mime_to_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/tiff" => "tiff",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

/// Infer the MIME type from the first bytes of an image payload.
///
/// Falls back to `"application/octet-stream"` if the magic bytes are
/// unrecognized.
pub fn mime_from_bytes(data: &[u8]) -> &'static str {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if data.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if data.starts_with(b"II\x2a\x00") || data.starts_with(b"MM\x00\x2a") {
        "image/tiff"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else if data.starts_with(b"BM") {
        "image/bmp"
    } else {
        "application/octet-stream"
    }
}

/// Per-leg outcome of a native clipboard write (for telemetry).
///
/// On Linux, every viable CLI backend for the session is attempted (see
/// `linux_write_tool_specs`). `cli_tools_tried` lists each tool invoked;
/// `cli_ok_tools` lists tools that returned Ok; `cli_ok` is true if any did.
#[derive(Debug, Clone, Default)]
pub struct NativeWriteOutcome {
    pub cli_tools_tried: Vec<&'static str>,
    /// Subset of `cli_tools_tried` that succeeded (order preserved).
    ///
    /// Tier caveat: on Wayland, wl-copy is read-back-verified only when
    /// data-control is absent; when `data_control && arboard_ok` its exit-0 is
    /// credited unverified (the arboard write is authoritative).
    pub cli_ok_tools: Vec<&'static str>,
    pub cli_ok: bool,
    pub arboard_ok: bool,
    /// The Wayland data-control protocol was available for this write (the
    /// environment probe, [`wayland_data_control_supported`]) — NOT proof the
    /// arboard write landed; a focus-free authoritative write additionally
    /// requires `arboard_ok`. Always false on macOS/Windows/X11.
    pub data_control: bool,
    /// True when at least one leg succeeded.
    pub any_ok: bool,
}

/// Write text and return per-leg outcomes for telemetry callers.
pub fn set_text_with_outcome(text: &str) -> NativeWriteOutcome {
    platform::set_text_with_outcome(text)
}

/// Write text to the system clipboard.
pub fn set_text(text: &str) -> anyhow::Result<()> {
    let outcome = platform::set_text_with_outcome(text);
    if outcome.any_ok {
        Ok(())
    } else {
        anyhow::bail!("no clipboard backend available")
    }
}

/// Copy an image file to the system clipboard.
///
/// On macOS, uses `osascript` to set the pasteboard from a file path.
/// On Linux, uses `wl-copy` or `xclip` if available.
/// On Windows, returns an error (not yet supported).
pub fn set_image_file(path: &std::path::Path) -> anyhow::Result<()> {
    platform::set_image_file(path)
}

/// The clipboard tool used for native writes on the current platform.
///
/// Returns `"pbcopy"` on macOS, `"arboard"` on Windows, and the probed CLI
/// tool name on Linux (or `"arboard"` if no CLI tool was found).
pub fn native_tool_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "pbcopy"
    }
    #[cfg(target_os = "linux")]
    {
        platform::linux_tool_spec().map_or("arboard", |spec| spec.name)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "arboard"
    }
}

/// Result of an explicit Wayland data-control capability probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WaylandDataControlProbe {
    /// The compositor definitively reported the protocol present or absent.
    Available(bool),
    /// The bounded worker did not answer before its deadline.
    Unavailable,
    /// The worker or compositor returned an operational error.
    Error(String),
}

/// Run one explicit, bounded Wayland data-control capability probe.
///
/// Unlike [`wayland_data_control_supported`], this preserves inconclusive
/// outcomes for diagnostics instead of mapping them to `false`.
pub fn probe_wayland_data_control() -> WaylandDataControlProbe {
    platform::probe_wayland_data_control()
}

/// Whether the Wayland compositor supports the data-control clipboard
/// protocol (`zwlr_data_control_v1` / `ext_data_control_v1`).
///
/// With data-control, arboard sets the selection compositor-side — no surface
/// and no focus required — so native copies survive the terminal losing focus
/// mid-copy. Without it (GNOME ≤ 47), arboard silently falls back to X11 via
/// the focus-mediated XWayland selection bridge and `wl-copy` uses its own
/// focus-dependent fallback, so writes need the terminal focused until
/// confirmed.
///
/// Memoized per process: definitive answers (the compositor reported the
/// protocol present or absent; kill switch; not a Wayland session) cache
/// forever, while an unanswered probe (timeout, connection failure) returns
/// `false` for the current call — fail closed — and is retried on a later
/// call, up to a small cap. Always `false` off Linux, off Wayland, or when
/// the `ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL` kill-switch env var is set. The kill
/// switch also disables the in-process arboard leg entirely on Wayland
/// sessions (see `arboard_wayland_bypassed`) — arboard's own backend
/// selection would otherwise still speak data-control to the compositor
/// regardless of this probe's answer.
pub fn wayland_data_control_supported() -> bool {
    platform::wayland_data_control_supported()
}

/// Error from [`wait_with_deadline`] when the deadline expired (child killed).
#[derive(Debug, thiserror::Error)]
#[error("process did not exit within {0:?}")]
pub struct WaitTimeout(pub std::time::Duration);

/// Wait for a child process, bounded by a deadline.
///
/// Polls `try_wait` (~15 ms interval); on expiry kills and reaps the child and
/// returns [`WaitTimeout`]. Clipboard helpers spawn tools (`wl-copy`, `xclip`,
/// `pbcopy`, `tmux load-buffer`) that can hang on a stuck compositor/server,
/// and an unbounded `wait()` would freeze the UI thread.
///
/// Callers must take/close `child.stdin` (or feed it from a file) before
/// waiting — unlike `wait()`, the `try_wait` loop does not drop stdin, so a
/// child still reading a held pipe would burn the whole deadline.
pub fn wait_with_deadline(
    child: &mut std::process::Child,
    deadline: std::time::Duration,
) -> anyhow::Result<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if start.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WaitTimeout(deadline).into());
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
}

/// Spool `data` to a temp file and return a read handle to feed a child's
/// stdin.
///
/// Clipboard tools may daemonize (wl-copy/xclip) or stall (wedged tmux
/// server), and a pipe write from the UI thread blocks once the payload
/// exceeds the pipe buffer (~64 KiB); a regular file is fully written before
/// spawn and needs no writer afterwards. The temp file is mode 0600 and
/// unlinked before this returns — the returned fd (and the child's dup of it)
/// stays readable.
pub fn spool_for_stdin(data: &[u8]) -> anyhow::Result<std::fs::File> {
    use anyhow::Context;
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new().context("spool clipboard payload to temp file")?;
    tmp.write_all(data)
        .context("spool clipboard payload to temp file")?;
    tmp.reopen().context("spool clipboard payload to temp file")
}

/// Build the OSC 52 clipboard-set byte sequence. When `tmux_passthrough` is
/// true, wrap in the tmux DCS passthrough envelope (only correct when tmux is
/// the IMMEDIATE terminal); otherwise emit a plain OSC 52.
fn osc52_sequence(text: &str, tmux_passthrough: bool) -> Vec<u8> {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    if tmux_passthrough {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\").into_bytes()
    } else {
        format!("\x1b]52;c;{encoded}\x07").into_bytes()
    }
}

/// Write text to the user's local clipboard via OSC 52 escape sequence.
///
/// Writes `\x1b]52;c;<base64>\x07` to stderr (the pager's terminal output
/// stream). Modern terminal emulators (iTerm2, Ghostty, Kitty, WezTerm,
/// Windows Terminal, Alacritty, etc.) interpret this to set their clipboard.
///
/// The caller decides `tmux_passthrough`: the tmux DCS passthrough envelope is
/// only correct when tmux is the IMMEDIATE terminal. Inside an editor
/// `:terminal` (Neovim/Vim/Emacs) the immediate emulator is the editor's
/// libvterm, not tmux, so the wrapper must not be used or it renders as
/// visible garbage. tmux ≥ 3.3a with `set -g set-clipboard on` passes OSC 52
/// through to the outer terminal; older tmux may need `set -g allow-passthrough on`.
pub fn set_text_osc52(text: &str, tmux_passthrough: bool) -> anyhow::Result<()> {
    use std::io::Write;

    let seq = osc52_sequence(text, tmux_passthrough);
    crate::stderr::with_locked_stderr(|stderr| -> std::io::Result<()> {
        stderr.write_all(&seq)?;
        stderr.flush()
    })?;
    Ok(())
}

/// Returns `true` when the process appears to be running inside a remote
/// SSH session (with or without a multiplexer like tmux/screen).
///
/// Checks for `SSH_CONNECTION`, `SSH_TTY`, or `SSH_CLIENT` environment
/// variables set by the OpenSSH server on the remote side.
pub fn is_remote_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}

/// Returns `true` when the process appears to be running inside a container
/// (Docker, Podman, Kubernetes, etc.) without a display server.
///
/// In this environment the native system clipboard (`arboard`) will fail
/// because there is no X11/Wayland compositor. OSC 52 terminal escapes are
/// the only viable clipboard path — they pass through the container's PTY
/// to the outer terminal emulator (e.g. Windows Terminal, iTerm2).
pub fn is_containerized_without_display() -> bool {
    // If a display server is available, native clipboard should work.
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return false;
    }

    // Docker creates this sentinel file in every container.
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }

    // Podman creates this file.
    if std::path::Path::new("/run/.containerenv").exists() {
        return true;
    }

    // Many container runtimes set this env var (podman, systemd-nspawn, etc.).
    if std::env::var_os("container").is_some() {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// macOS unified attachments `osascript` stdout parsing (pure, no I/O)
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "macos", test))]
mod attachments_protocol {
    pub const FURL_MARKER: &str = "<<<FURL>>>";
    pub const IMAGE_MARKER: &str = "<<<IMAGE>>>";

    /// Parse the stdout payload of the `get_file_urls` AppleScript.
    ///
    /// The script returns one of:
    /// - one or more POSIX paths separated by `\n` (success), or
    /// - the literal string `"none"` (no file URLs present), or
    /// - empty / whitespace-only (degenerate cases that map to `None`).
    pub fn parse_osascript_furl_output(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "none" {
            return None;
        }
        Some(trimmed.to_owned())
    }

    /// Parse stdout from the unified attachments AppleScript.
    ///
    /// Format:
    /// ```text
    /// <<<FURL>>>
    /// {none | one or more POSIX paths separated by newlines}
    /// <<<IMAGE>>>
    /// {NONE | PNGf | TIFF | JPEG}
    /// ```
    ///
    /// The image section is `NONE` when file URLs were found (image probe
    /// skipped) or when no image type is on the pasteboard.
    ///
    /// When both sections parse successfully, **file URLs take precedence** over
    /// the image class at the [`get_attachments`] layer (image bytes are not read
    /// if `file_urls` is `Some`).
    pub fn parse_attachments_output(raw: &str) -> (Option<String>, Option<&'static str>) {
        let (furl_section, image_line) = match raw.split_once(IMAGE_MARKER) {
            Some((before, after)) => (before, Some(after)),
            None => (raw, None),
        };
        let file_urls = if furl_section.contains(FURL_MARKER) {
            let furl_body = furl_section
                .strip_prefix(FURL_MARKER)
                .unwrap_or(furl_section)
                .trim_start_matches(['\n', '\r']);
            parse_osascript_furl_output(furl_body)
        } else {
            None
        };
        let image_class = image_line.and_then(|line| {
            line.trim()
                .strip_prefix("IMAGE:")
                .map(str::trim)
                .filter(|c| *c != "NONE" && !c.is_empty())
                .and_then(|c| match c {
                    "PNGf" => Some("PNGf"),
                    "TIFF" => Some("TIFF"),
                    "JPEG" => Some("JPEG"),
                    _ => None,
                })
        });
        if !raw.trim().is_empty() && file_urls.is_none() && image_class.is_none() {
            tracing::debug!(
                raw_len = raw.len(),
                has_furl_marker = raw.contains(FURL_MARKER),
                has_image_marker = raw.contains(IMAGE_MARKER),
                "parse_attachments_output: non-empty stdout did not parse to file URLs or image class",
            );
        }
        (file_urls, image_class)
    }
}

// ---------------------------------------------------------------------------
// macOS: subprocess-based clipboard (no AppKit linkage)
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod platform {
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    use super::attachments_protocol::{FURL_MARKER, IMAGE_MARKER, parse_attachments_output};
    use super::{ClipboardAttachments, ImageData};

    // -- Fast pasteboard probes (NSPasteboard via lazy dlopen) -------------
    //
    // These deliberately do NOT use `objc2-app-kit`: that crate emits a
    // `#[link]` against AppKit, and linking AppKit is exactly what this
    // module's pbcopy/pbpaste design avoids (dyld loads AppKit at startup
    // and its Metal/IOAccelerator init costs ~2 GB GPU memory in headless
    // processes — the leader runs from this same binary). Instead AppKit is
    // `dlopen`ed lazily at the FIRST probe and NSPasteboard is reached via
    // objc2 runtime messaging (libobjc only), so headless processes that
    // never probe never load AppKit at all.

    /// Load AppKit once so `objc_getClass("NSPasteboard")` can resolve.
    /// Returns false (probes unavailable) if the load fails.
    fn appkit_loaded() -> bool {
        static LOADED: OnceLock<bool> = OnceLock::new();
        *LOADED.get_or_init(|| {
            let path = c"/System/Library/Frameworks/AppKit.framework/AppKit";
            // SAFETY: dlopen with a constant NUL-terminated path; the handle
            // is intentionally leaked (AppKit stays loaded for the process).
            let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY) };
            !handle.is_null()
        })
    }

    pub(super) fn clipboard_prewarm() {
        // Framework load only (memoised); reads no pasteboard, so this is sound
        // to run on the off-UI-thread warm-up.
        let _ = appkit_loaded();
    }

    /// No Wayland on macOS.
    pub(super) fn wayland_data_control_supported() -> bool {
        false
    }

    pub(super) fn probe_wayland_data_control() -> super::WaylandDataControlProbe {
        super::WaylandDataControlProbe::Available(false)
    }

    /// Serializes every in-process NSPasteboard touch.
    ///
    /// The metadata-only design held "no concurrent in-process pasteboard
    /// access" by construction: all probes ran on the single UI thread and
    /// content reads were subprocesses. The paste-time native read
    /// ([`native_image_read`]) runs on a blocking-pool thread (the deferred
    /// probe effect) and can overlap the UI thread's focus/tick metadata
    /// probes — and AppKit reached via a bare `dlopen` (no NSApplication)
    /// is NOT safe against concurrent pasteboard messaging (parallel probe
    /// smoke tests crash with SIGSEGV/SIGABRT). The invariant is therefore
    /// now held by lock: every native pasteboard entry point takes this
    /// mutex for the duration of its autoreleasepool.
    static PASTEBOARD_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// The general pasteboard as a runtime-messaged object, or `None` when
    /// AppKit is unavailable.
    ///
    /// Thread-safety basis: objc2-app-kit does NOT classify NSPasteboard as
    /// MainThreadOnly (no MainThreadMarker on `generalPasteboard`, unlike
    /// NSView/NSWindow). Callers hold [`PASTEBOARD_LOCK`] so there is no
    /// concurrent in-process pasteboard access.
    fn general_pasteboard() -> Option<objc2::rc::Retained<objc2::runtime::AnyObject>> {
        if !appkit_loaded() {
            return None;
        }
        let cls = objc2::runtime::AnyClass::get(c"NSPasteboard")?;
        // SAFETY: +[NSPasteboard generalPasteboard] takes no arguments and
        // returns the shared pasteboard instance (nullable-safe via Option).
        unsafe { objc2::msg_send![cls, generalPasteboard] }
    }

    pub(super) fn clipboard_image_snapshot() -> (Option<u64>, bool) {
        let _guard = PASTEBOARD_LOCK.lock();
        objc2::rc::autoreleasepool(|_| {
            let Some(pb) = general_pasteboard() else {
                return (None, false);
            };
            // SAFETY: -[NSPasteboard changeCount] returns NSInteger; it is
            // monotonic and non-negative, so the cast to u64 is lossless.
            let count: isize = unsafe { objc2::msg_send![&*pb, changeCount] };
            // Collect the advertised identifiers, then classify with the
            // shared rule (`image_pasteable_from_types`): a raster type
            // only counts when no file-URL type is advertised alongside,
            // matching the paste-path priority (file URLs win).
            let has_image = advertised_types(&pb).is_some_and(|advertised| {
                super::image_pasteable_from_types(advertised.iter().map(|t| t.as_slice()))
            });
            (Some(count as u64), has_image)
        })
    }

    pub(super) fn clipboard_change_count() -> Option<u64> {
        let _guard = PASTEBOARD_LOCK.lock();
        objc2::rc::autoreleasepool(|_| {
            let pb = general_pasteboard()?;
            // SAFETY: -[NSPasteboard changeCount] returns a monotonic,
            // non-negative NSInteger; the cast to u64 is lossless. This messages
            // ONLY changeCount — no `types` scan, no data read — so it is the
            // cheapest possible pasteboard touch for the throttled poll.
            let count: isize = unsafe { objc2::msg_send![&*pb, changeCount] };
            Some(count as u64)
        })
    }

    /// Advertised pasteboard type identifiers as raw byte strings.
    ///
    /// `None` when AppKit is unavailable or `types` returns nil. Shared by
    /// the snapshot probe and the native paste-time read so both classify
    /// the same advertised list with `image_pasteable_from_types`.
    fn advertised_types(pb: &objc2::runtime::AnyObject) -> Option<Vec<Vec<u8>>> {
        // SAFETY: -[NSPasteboard types] returns a nullable
        // NSArray<NSPasteboardType>; only count/objectAtIndex/UTF8String
        // are messaged on it, all data-free.
        let types: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
            unsafe { objc2::msg_send![pb, types] };
        let types = types?;
        let n: usize = unsafe { objc2::msg_send![&*types, count] };
        let mut advertised: Vec<Vec<u8>> = Vec::with_capacity(n);
        for i in 0..n {
            let ty: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
                unsafe { objc2::msg_send![&*types, objectAtIndex: i] };
            let Some(ty) = ty else { continue };
            let utf8: *const std::os::raw::c_char = unsafe { objc2::msg_send![&*ty, UTF8String] };
            if utf8.is_null() {
                continue;
            }
            // SAFETY: UTF8String is NUL-terminated and outlives this
            // iteration (the owning NSString is retained above).
            let bytes = unsafe { std::ffi::CStr::from_ptr(utf8) }.to_bytes();
            advertised.push(bytes.to_vec());
        }
        Some(advertised)
    }

    /// In-process pasteboard image read via the lazily `dlopen`ed AppKit.
    ///
    /// The paste hot path: when a raster type is advertised with no
    /// file-URL type alongside (`native_image_type_from_types`), read the
    /// encoded bytes with `-[NSPasteboard dataForType:]` — no subprocess, no
    /// temp file, no AppleScript coercion. Returns `None` for every other
    /// pasteboard shape (file URLs present, no raster, AppKit unavailable,
    /// nil/empty data) so callers fall back to the unchanged `osascript`
    /// path, and `None` when `ECHO_AGENT_CLIPBOARD_NO_NATIVE_READ` is set (kill
    /// switch if a future macOS gates `dataForType:` behind a privacy
    /// prompt; the focus/tick probes stay metadata-only either way).
    ///
    /// Thread-safety basis matches [`general_pasteboard`]: NSPasteboard is
    /// not MainThreadOnly, and only `types` + `dataForType:` are messaged.
    /// The deferred paste probe calls this from a blocking-pool thread, the
    /// same off-main pattern `clipboard_prewarm` already established.
    pub(super) fn native_image_read() -> Option<super::ImageData> {
        if std::env::var_os("ECHO_AGENT_CLIPBOARD_NO_NATIVE_READ").is_some() {
            return None;
        }
        let _guard = PASTEBOARD_LOCK.lock();
        objc2::rc::autoreleasepool(|_| {
            let pb = general_pasteboard()?;
            let advertised = advertised_types(&pb)?;
            let (uti, mime) = super::native_image_type_from_types(&advertised)?;
            let uti_cstring = std::ffi::CString::new(uti).ok()?;
            let cls = objc2::runtime::AnyClass::get(c"NSString")?;
            // SAFETY: +[NSString stringWithUTF8String:] takes a NUL-terminated
            // C string (valid for the duration of the call) and returns a
            // nullable autoreleased NSString.
            let ns_type: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
                unsafe { objc2::msg_send![cls, stringWithUTF8String: uti_cstring.as_ptr()] };
            let ns_type = ns_type?;
            // SAFETY: -[NSPasteboard dataForType:] takes an NSPasteboardType
            // (NSString) and returns a nullable NSData; only requested for a
            // type the pasteboard itself advertised.
            let data: Option<objc2::rc::Retained<objc2::runtime::AnyObject>> =
                unsafe { objc2::msg_send![&*pb, dataForType: &*ns_type] };
            let data = data?;
            // SAFETY: -[NSData length] returns NSUInteger.
            let len: usize = unsafe { objc2::msg_send![&*data, length] };
            if len == 0 {
                return None;
            }
            // SAFETY: -[NSData bytes] returns a pointer valid for `len` bytes
            // while `data` is retained (it is, until the end of this scope);
            // the bytes are copied out immediately.
            let bytes_ptr: *const std::os::raw::c_void = unsafe { objc2::msg_send![&*data, bytes] };
            if bytes_ptr.is_null() {
                return None;
            }
            let mut buf = vec![0u8; len];
            // SAFETY: source is valid for `len` reads (NSData contract),
            // destination is a fresh Vec of exactly `len` bytes, and the
            // regions cannot overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes_ptr as *const u8, buf.as_mut_ptr(), len);
            }
            Some(super::ImageData {
                data: buf,
                mime_type: mime.to_owned(),
            })
        })
    }

    pub(super) fn checked_command_stdout(
        label: &str,
        output: std::io::Result<std::process::Output>,
    ) -> anyhow::Result<Vec<u8>> {
        let output = output.map_err(|error| anyhow::anyhow!("failed to run {label}: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("{label} exited with {}: {}", output.status, stderr.trim());
        }
        Ok(output.stdout)
    }

    fn attachments_probe_temp_paths() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)
    {
        let temp_dir = std::env::temp_dir();
        (
            temp_dir.join("echo-agent-clipboard-probe.png"),
            temp_dir.join("echo-agent-clipboard-probe.tiff"),
            temp_dir.join("echo-agent-clipboard-probe.jpg"),
        )
    }

    fn read_clipboard_image_from_class(
        class: &str,
        path_png: &std::path::Path,
        path_tiff: &std::path::Path,
        path_jpg: &std::path::Path,
    ) -> anyhow::Result<Option<ImageData>> {
        let (temp_path, mime) = match class {
            "PNGf" => (path_png, "image/png"),
            "TIFF" => (path_tiff, "image/tiff"),
            "JPEG" => (path_jpg, "image/jpeg"),
            _ => return Ok(None),
        };

        let data = match std::fs::read(temp_path) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) => {
                let _ = std::fs::remove_file(temp_path);
                return Ok(None);
            }
            Err(e) => {
                let _ = std::fs::remove_file(temp_path);
                return Err(anyhow::anyhow!("failed to read clipboard temp file: {e}"));
            }
        };

        let _ = std::fs::remove_file(temp_path);
        Ok(Some(ImageData {
            data,
            mime_type: mime.to_owned(),
        }))
    }

    fn remove_attachment_probe_temps(
        path_png: &std::path::Path,
        path_tiff: &std::path::Path,
        path_jpg: &std::path::Path,
    ) {
        let _ = std::fs::remove_file(path_png);
        let _ = std::fs::remove_file(path_tiff);
        let _ = std::fs::remove_file(path_jpg);
    }

    fn attachments_osascript(
        path_png: &std::path::Path,
        path_tiff: &std::path::Path,
        path_jpg: &std::path::Path,
    ) -> String {
        format!(
            "set furlOut to \"none\"\n\
             try\n\
             set urlList to the clipboard as list\n\
             set out to \"\"\n\
             repeat with u in urlList\n\
             try\n\
             set itemRef to contents of u as \u{00AB}class furl\u{00BB}\n\
             set out to out & POSIX path of itemRef & \"\\n\"\n\
             end try\n\
             end repeat\n\
             if out is not \"\" then\n\
             set furlOut to out\n\
             else\n\
             try\n\
             set urlRef to the clipboard as \u{00AB}class furl\u{00BB}\n\
             set furlOut to POSIX path of urlRef\n\
             on error\n\
             end try\n\
             end if\n\
             on error\n\
             try\n\
             set urlRef to the clipboard as \u{00AB}class furl\u{00BB}\n\
             set furlOut to POSIX path of urlRef\n\
             on error\n\
             end try\n\
             end try\n\
             set imageOut to \"NONE\"\n\
             if furlOut is \"none\" then\n\
             try\n\
             set imgData to the clipboard as \u{00AB}class PNGf\u{00BB}\n\
             set filePath to POSIX file \"{png}\" as text\n\
             set fRef to open for access file filePath with write permission\n\
             set eof of fRef to 0\n\
             write imgData to fRef\n\
             close access fRef\n\
             set imageOut to \"PNGf\"\n\
             on error\n\
             try\n\
             set imgData to the clipboard as \u{00AB}class TIFF\u{00BB}\n\
             set filePath to POSIX file \"{tiff}\" as text\n\
             set fRef to open for access file filePath with write permission\n\
             set eof of fRef to 0\n\
             write imgData to fRef\n\
             close access fRef\n\
             set imageOut to \"TIFF\"\n\
             on error\n\
             try\n\
             set imgData to the clipboard as \u{00AB}class JPEG\u{00BB}\n\
             set filePath to POSIX file \"{jpg}\" as text\n\
             set fRef to open for access file filePath with write permission\n\
             set eof of fRef to 0\n\
             write imgData to fRef\n\
             close access fRef\n\
             set imageOut to \"JPEG\"\n\
             on error\n\
             end try\n\
             end try\n\
             end try\n\
             end if\n\
             return \"{furl_marker}\" & linefeed & furlOut & linefeed & \"{image_marker}\" & linefeed & \"IMAGE:\" & imageOut",
            png = path_png.display(),
            tiff = path_tiff.display(),
            jpg = path_jpg.display(),
            furl_marker = FURL_MARKER,
            image_marker = IMAGE_MARKER,
        )
    }

    fn run_attachments_osascript() -> anyhow::Result<String> {
        let (path_png, path_tiff, path_jpg) = attachments_probe_temp_paths();
        remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);

        let script = attachments_osascript(&path_png, &path_tiff, &path_jpg);

        let mut cmd = Command::new("osascript");
        cmd.arg("-e")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        let stdout = match checked_command_stdout("osascript", cmd.output()) {
            Ok(stdout) => stdout,
            Err(error) => {
                remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);
                return Err(error);
            }
        };
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    }

    /// Unified furl-then-image pasteboard probe.
    ///
    /// Hot path first: a raster advertised with no file-URL type is read
    /// in-process (`native_image_read`, no subprocess / temp file). This is
    /// reachable only when the pasteboard text was empty or unactionable, so
    /// the text→`furl` coercions the AppleScript performs cannot apply — a
    /// furl can only come from an advertised file-URL type, which routes to
    /// the `osascript` below exactly as before.
    pub fn get_attachments() -> anyhow::Result<ClipboardAttachments> {
        if let Some(image) = native_image_read() {
            return Ok(ClipboardAttachments {
                file_urls: None,
                image: Some(image),
            });
        }
        let raw = run_attachments_osascript()?;
        if raw.trim().is_empty() {
            return Ok(ClipboardAttachments::default());
        }

        let (path_png, path_tiff, path_jpg) = attachments_probe_temp_paths();
        let (file_urls, image_class) = parse_attachments_output(&raw);
        let image = if file_urls.is_some() {
            remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);
            None
        } else {
            match image_class {
                Some(class) => {
                    read_clipboard_image_from_class(class, &path_png, &path_tiff, &path_jpg)?
                }
                None => {
                    remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);
                    None
                }
            }
        };
        Ok(ClipboardAttachments { file_urls, image })
    }

    /// Read text via `pbpaste -Prefer txt`.
    ///
    /// The `-Prefer txt` flag ensures we only get plain-text content, matching
    /// the behaviour of `arboard::Clipboard::get_text()`.
    pub fn get_text() -> anyhow::Result<Option<String>> {
        let mut cmd = Command::new("pbpaste");
        cmd.arg("-Prefer")
            .arg("txt")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        let stdout = checked_command_stdout("pbpaste", cmd.output())?;
        if stdout.is_empty() {
            return Ok(None);
        }

        Ok(Some(String::from_utf8_lossy(&stdout).into_owned()))
    }

    pub fn set_text_with_outcome(text: &str) -> super::NativeWriteOutcome {
        let mut outcome = super::NativeWriteOutcome {
            cli_tools_tried: vec!["pbcopy"],
            ..Default::default()
        };
        let result = (|| -> anyhow::Result<()> {
            // Spooled stdin (not a pipe): a stalled pbcopy must not block the
            // UI thread on the write, and the deadline wait needs stdin closed.
            let stdin = super::spool_for_stdin(text.as_bytes())?;
            let mut cmd = Command::new("pbcopy");
            cmd.stdin(Stdio::from(stdin))
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            echo_agent_tools::util::detach_std_command(&mut cmd);
            #[allow(clippy::disallowed_methods)] // short-lived clipboard helper, waited on below
            let mut child = cmd
                .spawn()
                .map_err(|e| anyhow::anyhow!("failed to spawn pbcopy: {e}"))?;
            let deadline = std::time::Duration::from_secs(2);
            let status = super::wait_with_deadline(&mut child, deadline)?;
            if !status.success() {
                anyhow::bail!("pbcopy exited with status {status}");
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                outcome.cli_ok = true;
                outcome.cli_ok_tools.push("pbcopy");
                outcome.any_ok = true;
            }
            Err(e) => tracing::debug!("pbcopy failed: {e}"),
        }
        outcome
    }

    /// Read an image from the macOS clipboard via `osascript`.
    ///
    /// Probes PNG, TIFF, then JPEG in a single `osascript` invocation
    /// using nested `try` blocks. This avoids spawning up to 3 separate
    /// subprocesses when no image is present, reducing worst-case latency
    /// from ~300-600 ms to ~100-200 ms.
    ///
    /// Uses a temp file as the transfer medium to avoid brittle hex
    /// parsing of AppleScript output.
    pub fn get_image() -> anyhow::Result<Option<ImageData>> {
        // Hot path: raster advertised with no file-URL type — in-process
        // read, no subprocess. Any other shape (including the Copy-Image
        // caption case where only legacy raster spellings are advertised)
        // falls through to the AppleScript coercion below unchanged.
        if let Some(image) = native_image_read() {
            return Ok(Some(image));
        }

        let (path_png, path_tiff, path_jpg) = attachments_probe_temp_paths();
        remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);

        // Image-only AppleScript (ImageOnly paste route). Unicode guillemets
        // (\u{AB}/\u{BB}) are required for `«class …»` in `osascript -e`.
        let script = format!(
            "try\n\
             set imgData to the clipboard as \u{00AB}class PNGf\u{00BB}\n\
             set filePath to POSIX file \"{png}\" as text\n\
             set fRef to open for access file filePath with write permission\n\
             set eof of fRef to 0\n\
             write imgData to fRef\n\
             close access fRef\n\
             return \"PNGf\"\n\
             on error\n\
             try\n\
             set imgData to the clipboard as \u{00AB}class TIFF\u{00BB}\n\
             set filePath to POSIX file \"{tiff}\" as text\n\
             set fRef to open for access file filePath with write permission\n\
             set eof of fRef to 0\n\
             write imgData to fRef\n\
             close access fRef\n\
             return \"TIFF\"\n\
             on error\n\
             try\n\
             set imgData to the clipboard as \u{00AB}class JPEG\u{00BB}\n\
             set filePath to POSIX file \"{jpg}\" as text\n\
             set fRef to open for access file filePath with write permission\n\
             set eof of fRef to 0\n\
             write imgData to fRef\n\
             close access fRef\n\
             return \"JPEG\"\n\
             on error\n\
             return \"none\"\n\
             end try\n\
             end try\n\
             end try",
            png = path_png.display(),
            tiff = path_tiff.display(),
            jpg = path_jpg.display(),
        );

        let mut cmd = Command::new("osascript");
        cmd.arg("-e")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        let stdout = match checked_command_stdout("osascript", cmd.output()) {
            Ok(stdout) => stdout,
            Err(error) => {
                remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);
                return Err(error);
            }
        };
        let class = String::from_utf8_lossy(&stdout);
        let class = class.trim();
        if class == "none" {
            remove_attachment_probe_temps(&path_png, &path_tiff, &path_jpg);
            return Ok(None);
        }
        read_clipboard_image_from_class(class, &path_png, &path_tiff, &path_jpg)
    }

    /// Read file URLs from the macOS pasteboard via `osascript`.
    ///
    /// Probes for the `«class furl»` (file URL) pasteboard type.
    /// macOS Finder's `Cmd+C` places this type for every selected
    /// file, even when `public.utf8-plain-text` is empty or absent.
    ///
    /// **Ordering note**: the AppleScript tries the LIST coercion
    /// first (`the clipboard as list`) and iterates it, only falling
    /// back to the single-`«class furl»` coercion when the list path
    /// errors. This is intentional: on several macOS versions
    /// `the clipboard as «class furl»` on a multi-file selection
    /// silently returns just the first file's URL instead of erroring
    /// (so the documented "list fallback" never fires and only one
    /// file is recovered). List-first guarantees all N files are
    /// captured in the common multi-file Cmd+C case.
    ///
    /// Inside the list iteration each item is coerced via
    /// `as «class furl»` so non-furl items (text-only clipboards
    /// that still coerce to a single-item list) are skipped rather
    /// than passed through as bogus "paths".
    ///
    /// Returns `Ok(None)` when the pasteboard has no file URLs.
    pub fn get_file_urls() -> anyhow::Result<Option<String>> {
        Ok(get_attachments()?.file_urls)
    }

    /// Map pasteboard class to file extension for the temp file.
    #[cfg(test)]
    pub(super) fn extension_for_class(class: &str) -> &'static str {
        match class {
            "PNGf" => "png",
            "TIFF" => "tiff",
            "JPEG" | "JPEGAufs" => "jpg",
            _ => "bin",
        }
    }

    /// Copy an image file to the macOS clipboard via `osascript`.
    ///
    /// Detects the pasteboard class from the file extension (PNG, JPEG, TIFF).
    /// Falls back to PNG if the extension is unrecognized.
    pub fn set_image_file(path: &std::path::Path) -> anyhow::Result<()> {
        let class = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg" | "jpeg") => "JPEG",
            Some("tiff" | "tif") => "TIFF",
            _ => "PNGf",
        };
        let path_str = path.display().to_string().replace('"', "\\\"");
        let script = format!(
            "set the clipboard to (read (POSIX file \"{path_str}\") as \u{00AB}class {class}\u{00BB})"
        );
        let mut cmd = Command::new("osascript");
        cmd.arg("-e")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        let output = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("failed to run osascript: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("osascript failed: {stderr}");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Linux / Windows: arboard with CLI-tool fallback on Linux
// ---------------------------------------------------------------------------
#[cfg(not(target_os = "macos"))]
mod platform {
    use super::ImageData;
    use std::process::{Command, Stdio};

    /// No subprocess-free pasteboard probe exists off-macOS.
    pub(super) fn clipboard_image_snapshot() -> (Option<u64>, bool) {
        (None, false)
    }

    /// No native pasteboard changeCount off-macOS.
    pub(super) fn clipboard_change_count() -> Option<u64> {
        None
    }

    /// No AppKit to pre-warm off-macOS.
    pub(super) fn clipboard_prewarm() {}

    // -- arboard helpers (the in-process leg on all non-macOS platforms) ------

    /// Run `f` on a named worker thread and wait up to `deadline` for its
    /// result. `Err(Timeout)` abandons the worker (it stays parked on the
    /// blocked call — a leaked thread beats a frozen UI); `Err(Disconnected)`
    /// means the worker died before sending (spawn failure or panic).
    fn spawn_with_deadline<T: Send + 'static>(
        name: &str,
        deadline: std::time::Duration,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, std::sync::mpsc::RecvTimeoutError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _ = tx.send(f());
            });
        if spawned.is_err() {
            return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
        }
        rx.recv_timeout(deadline)
    }

    /// Memoized read of the `ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL` kill switch —
    /// the single env-var site that both gates (the data-control probe and
    /// the arboard bypass) consume, so they can never drift apart.
    #[cfg(target_os = "linux")]
    fn data_control_kill_switch_set() -> bool {
        static SET: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SET.get_or_init(|| std::env::var_os("ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL").is_some())
    }

    /// True when the arboard leg must be skipped entirely: on Wayland the
    /// `ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL` kill switch has to stop arboard from
    /// speaking the data-control protocol at all (arboard picks that backend
    /// on its own whenever `WAYLAND_DISPLAY` is set — there is no way to force
    /// it back onto X11), so copies/pastes ride the CLI tools instead.
    fn arboard_wayland_bypassed() -> bool {
        #[cfg(target_os = "linux")]
        {
            data_control_kill_switch_set() && env_present("WAYLAND_DISPLAY")
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    /// Deadline for opening an in-process display connection, shared by the
    /// data-control probe and arboard `Clipboard::new()` (which performs the
    /// same connect): with separate budgets the probe could time out while the
    /// init then succeeds, recording `data_control = false` for writes that do
    /// go through data-control.
    const DISPLAY_CONN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

    /// Process-global arboard instance (the clipboard "lease"), created lazily
    /// on first write and kept alive for the process lifetime.
    ///
    /// On X11 the selection is served only while an instance is alive, and
    /// dropping the last one pays a ~100 ms clipboard-manager handover (content
    /// lost without a manager) — the old per-copy instances paid that on every
    /// copy. On Wayland data-control the backend's detached serving thread
    /// stays rooted. Initialization runs on a bounded worker
    /// (`spawn_with_deadline`); failure, timeout, or the Wayland kill switch
    /// (env vars don't change at runtime) is cached as `None`. When the X11
    /// backend degrades ("handler thread ... stopped" errors from `set_text`)
    /// the degradation is permanent for this process; writes keep failing fast
    /// and the CLI legs in `set_text_with_outcome` remain the write path.
    ///
    /// The mutex is only held for the in-process arboard call, never across
    /// subprocess invocations (a `set_text` blocked on a compositor that hung
    /// *after* init remains unbounded by design — full async copy is a larger
    /// refactor). Reads do NOT take the lease: they run on abandonable worker
    /// threads (`arboard_read_with_deadline`) whose own short-lived instances
    /// are cheap while the lease keeps the shared X11 context alive.
    fn arboard_lease() -> anyhow::Result<&'static parking_lot::Mutex<arboard::Clipboard>> {
        static LEASE: std::sync::OnceLock<Option<parking_lot::Mutex<arboard::Clipboard>>> =
            std::sync::OnceLock::new();
        LEASE
            .get_or_init(|| {
                if arboard_wayland_bypassed() {
                    tracing::debug!("arboard leg disabled (ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL)");
                    return None;
                }
                match spawn_with_deadline(
                    "clipboard-init",
                    DISPLAY_CONN_WAIT,
                    arboard::Clipboard::new,
                ) {
                    Ok(Ok(clipboard)) => Some(parking_lot::Mutex::new(clipboard)),
                    Ok(Err(e)) => {
                        tracing::debug!("arboard Clipboard::new failed: {e}");
                        None
                    }
                    Err(e) => {
                        tracing::debug!("arboard Clipboard::new did not complete: {e}");
                        None
                    }
                }
            })
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("arboard unavailable"))
    }

    /// Deadline for in-process arboard reads. The Wayland data-control read has
    /// no internal timeout and blocks forever on a hung selection owner (the
    /// X11 path has a 4 s budget), so reads run on a worker thread that is
    /// abandoned on expiry. The worker's `Clipboard` instance leaks with it;
    /// harmless while the lease keeps the shared backend alive.
    const ARBOARD_READ_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

    fn arboard_read_with_deadline<T: Send + 'static>(
        op: impl FnOnce(&mut arboard::Clipboard) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        use std::sync::mpsc::RecvTimeoutError;
        if arboard_wayland_bypassed() {
            anyhow::bail!("arboard leg disabled (ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL)");
        }
        let result = spawn_with_deadline("clipboard-read", ARBOARD_READ_WAIT, move || {
            arboard::Clipboard::new()
                .map_err(anyhow::Error::from)
                .and_then(|mut clipboard| op(&mut clipboard))
        });
        match result {
            Ok(inner) => inner,
            Err(RecvTimeoutError::Timeout) => Err(anyhow::anyhow!("arboard read timed out")),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!("arboard read worker died")),
        }
    }

    fn arboard_get_text() -> anyhow::Result<Option<String>> {
        arboard_read_with_deadline(|clipboard| match clipboard.get_text() {
            Ok(text) if text.is_empty() => Ok(None),
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(err) => Err(err.into()),
        })
    }

    #[cfg(target_os = "linux")]
    fn arboard_get_primary_text() -> anyhow::Result<Option<String>> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        arboard_read_with_deadline(|clipboard| {
            match clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
            {
                Ok(text) if text.is_empty() => Ok(None),
                Ok(text) => Ok(Some(text)),
                Err(
                    arboard::Error::ContentNotAvailable | arboard::Error::ClipboardNotSupported,
                ) => Ok(None),
                Err(err) => Err(err.into()),
            }
        })
    }

    fn arboard_set_text(text: &str) -> anyhow::Result<()> {
        arboard_lease()?.lock().set_text(text)?;
        Ok(())
    }

    fn arboard_get_image() -> anyhow::Result<Option<ImageData>> {
        arboard_read_with_deadline(|clipboard| {
            let img_data = match clipboard.get_image() {
                Ok(data) => data,
                Err(arboard::Error::ContentNotAvailable) => return Ok(None),
                Err(err) => return Err(err.into()),
            };

            let png_bytes = encode_rgba_to_png(
                &img_data.bytes,
                img_data.width as u32,
                img_data.height as u32,
            )?;

            Ok(Some(ImageData {
                data: png_bytes,
                mime_type: "image/png".to_owned(),
            }))
        })
    }

    // -- Linux CLI tools ------------------------------------------------------
    //
    // arboard is built with `wayland-data-control`: on compositors exposing the
    // data-control protocol (probe: `wayland_data_control_supported`) it sets
    // the Wayland selection focus-free and the arboard write is authoritative;
    // on older compositors (GNOME ≤ 47) it silently falls back to X11/XWayland
    // and the CLI tools carry the write (see `set_text_with_outcome`). Reads
    // shell out when arboard fails, or when its "empty" answer is not
    // authoritative on Wayland (see `wayland_tool_selected`).
    //
    // Tools verified:
    //   Wayland: wl-copy / wl-paste (wl-clipboard package, v2.3+)
    //   X11:     xclip -selection clipboard / xsel --clipboard

    /// Argv specs for each Linux clipboard tool. All CLI dispatch goes
    /// through `run_pipe_in` / `run_capture_out` with these specs.
    #[cfg(target_os = "linux")]
    pub(super) struct ToolSpec {
        pub(super) name: &'static str,
        /// Tool reads the Wayland selection (not the X11 CLIPBOARD arboard checks).
        reads_wayland_selection: bool,
        write_text: &'static [&'static str],
        read_text: &'static [&'static str],
        read_primary: Option<&'static [&'static str]>,
        write_png: Option<&'static [&'static str]>,
        read_png: Option<&'static [&'static str]>,
    }

    #[cfg(target_os = "linux")]
    const WL_SPEC: ToolSpec = ToolSpec {
        name: "wl-copy",
        reads_wayland_selection: true,
        // `-t text`: exit non-zero on non-text clipboards instead of dumping
        // raw bytes of an arbitrary MIME type. `--no-newline` makes wl-paste
        // return the selection verbatim, which `wayland_readback_matches` compares exactly.
        read_text: &["wl-paste", "--no-newline", "-t", "text"],
        write_text: &["wl-copy"],
        read_primary: None,
        write_png: Some(&["wl-copy", "-t", "image/png"]),
        read_png: Some(&["wl-paste", "-t", "image/png"]),
    };

    #[cfg(target_os = "linux")]
    const XCLIP_SPEC: ToolSpec = ToolSpec {
        name: "xclip",
        reads_wayland_selection: false,
        write_text: &["xclip", "-selection", "clipboard"],
        read_text: &["xclip", "-o", "-selection", "clipboard"],
        read_primary: Some(&["xclip", "-o", "-selection", "primary"]),
        write_png: Some(&["xclip", "-selection", "clipboard", "-t", "image/png", "-i"]),
        read_png: Some(&["xclip", "-selection", "clipboard", "-t", "image/png", "-o"]),
    };

    #[cfg(target_os = "linux")]
    const XSEL_SPEC: ToolSpec = ToolSpec {
        name: "xsel",
        reads_wayland_selection: false,
        write_text: &["xsel", "--clipboard", "--input"],
        read_text: &["xsel", "--clipboard", "--output"],
        read_primary: Some(&["xsel", "--primary", "--output"]),
        write_png: None, // xsel doesn't support typed clipboard
        read_png: None,
    };

    /// Cached tool spec, probed once at first use.
    #[cfg(target_os = "linux")]
    pub(super) fn linux_tool_spec() -> Option<&'static ToolSpec> {
        static SPEC: std::sync::OnceLock<Option<&'static ToolSpec>> = std::sync::OnceLock::new();
        *SPEC.get_or_init(probe_tool_spec)
    }

    /// On Wayland-only sessions (no focused X11/XWayland client) the X11
    /// CLIPBOARD has no owner, so arboard's `Ok(None)` is not authoritative
    /// and `wl-paste` must be consulted. `xclip`/`xsel` re-read the same X11
    /// selection, so they don't qualify.
    ///
    /// Known limitation: a lingering X11 CLIPBOARD owner makes arboard return
    /// `Ok(Some(stale))`, shadowing the Wayland selection.
    #[cfg(target_os = "linux")]
    fn wayland_tool_selected(spec: Option<&ToolSpec>) -> bool {
        spec.is_some_and(|spec| spec.reads_wayland_selection)
    }

    /// Indefinite probe outcomes tolerated before deciding `false` permanently
    /// (mirrors the lease's timeout ⇒ permanent-degradation stance), so a
    /// wedged compositor can't tax every copy with a bounded probe forever.
    #[cfg(target_os = "linux")]
    const PROBE_INDEFINITE_MAX: u32 = 3;

    /// Probe cache: `decided` is the permanent answer once set;
    /// `indefinite_seen` counts completed unanswered probes toward the cap.
    #[cfg(target_os = "linux")]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ProbeCache {
        decided: Option<bool>,
        indefinite_seen: u32,
    }

    #[cfg(target_os = "linux")]
    impl ProbeCache {
        const fn new() -> Self {
            Self {
                decided: None,
                indefinite_seen: 0,
            }
        }
    }

    /// Apply one probe outcome to the cache and return the answer for this
    /// call. Definitive answers decide permanently; indefinite outcomes fail
    /// closed and only decide (as `false`) once the retry cap is exhausted —
    /// deciding earlier would be a permanent false negative: the lease init
    /// runs the same connect on the same budget and may succeed moments later,
    /// leaving every working data-control copy mis-toasted as failed.
    #[cfg(target_os = "linux")]
    fn apply_probe_outcome(
        cache: &mut ProbeCache,
        outcome: &super::WaylandDataControlProbe,
    ) -> bool {
        match outcome {
            super::WaylandDataControlProbe::Available(supported) => {
                cache.decided = Some(*supported);
                *supported
            }
            super::WaylandDataControlProbe::Unavailable
            | super::WaylandDataControlProbe::Error(_) => {
                cache.indefinite_seen += 1;
                if cache.indefinite_seen >= PROBE_INDEFINITE_MAX {
                    cache.decided = Some(false);
                }
                false
            }
        }
    }

    /// Testable core of the memoized probe: return the decided answer, or run
    /// `probe` and apply its outcome.
    #[cfg(target_os = "linux")]
    fn cached_probe_answer(
        cache: &parking_lot::Mutex<ProbeCache>,
        probe: impl FnOnce() -> super::WaylandDataControlProbe,
    ) -> bool {
        let mut cache = cache.lock();
        if let Some(decided) = cache.decided {
            return decided;
        }
        // Probing while holding the lock serializes concurrent callers (UI
        // copy vs. the spawn_blocking telemetry snapshot): the second waits
        // for the first's answer — bounded by the probe's own
        // DISPLAY_CONN_WAIT deadline — instead of double-probing, racing the
        // cap count past PROBE_INDEFINITE_MAX in one wall-clock burst, or
        // dropping a concurrent definitive answer.
        let outcome = probe();
        apply_probe_outcome(&mut cache, &outcome)
    }

    /// Memoized data-control probe (see the public wrapper for semantics).
    #[cfg(target_os = "linux")]
    pub(super) fn wayland_data_control_supported() -> bool {
        static CACHE: parking_lot::Mutex<ProbeCache> = parking_lot::Mutex::new(ProbeCache::new());
        // Env gates are runtime-constant: no lock and no probe. The kill
        // switch (precedent: ECHO_AGENT_CLIPBOARD_NO_NATIVE_READ) and non-Wayland
        // sessions are always "no data-control".
        if data_control_kill_switch_set() || !env_present("WAYLAND_DISPLAY") {
            return false;
        }
        cached_probe_answer(&CACHE, probe_data_control)
    }

    /// No Wayland off Linux.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn wayland_data_control_supported() -> bool {
        false
    }

    #[cfg(target_os = "linux")]
    pub(super) fn probe_wayland_data_control() -> super::WaylandDataControlProbe {
        if data_control_kill_switch_set() || !env_present("WAYLAND_DISPLAY") {
            return super::WaylandDataControlProbe::Available(false);
        }
        probe_data_control()
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn probe_wayland_data_control() -> super::WaylandDataControlProbe {
        super::WaylandDataControlProbe::Available(false)
    }

    /// Run the same compositor probe arboard's `Clipboard::new` uses to pick
    /// its Wayland backend, on a bounded worker. Both the legacy cached bool
    /// adapter and explicit diagnostics consume this typed result.
    #[cfg(target_os = "linux")]
    fn probe_data_control() -> super::WaylandDataControlProbe {
        use std::sync::mpsc::RecvTimeoutError;
        match spawn_with_deadline(
            "clipboard-dc-probe",
            DISPLAY_CONN_WAIT,
            wl_clipboard_rs::utils::is_primary_selection_supported,
        ) {
            Ok(result) => data_control_from_result(result),
            Err(RecvTimeoutError::Timeout) => super::WaylandDataControlProbe::Unavailable,
            Err(RecvTimeoutError::Disconnected) => {
                super::WaylandDataControlProbe::Error("probe worker died".to_owned())
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum DataControlSemanticResult {
        Present,
        MissingProtocol,
        NoSeats,
        ConnectionUnavailable,
    }

    #[cfg(target_os = "linux")]
    fn classify_data_control_semantic(
        result: DataControlSemanticResult,
    ) -> super::WaylandDataControlProbe {
        use super::WaylandDataControlProbe;
        match result {
            DataControlSemanticResult::Present => WaylandDataControlProbe::Available(true),
            DataControlSemanticResult::MissingProtocol => WaylandDataControlProbe::Available(false),
            DataControlSemanticResult::NoSeats
            | DataControlSemanticResult::ConnectionUnavailable => {
                WaylandDataControlProbe::Unavailable
            }
        }
    }

    /// Canonical exhaustive mapping from dependency errors to semantic classes.
    #[cfg(target_os = "linux")]
    fn data_control_from_result(
        result: Result<bool, wl_clipboard_rs::utils::PrimarySelectionCheckError>,
    ) -> super::WaylandDataControlProbe {
        use wl_clipboard_rs::utils::PrimarySelectionCheckError;
        let semantic = match result {
            Ok(_) => DataControlSemanticResult::Present,
            Err(PrimarySelectionCheckError::MissingProtocol) => {
                DataControlSemanticResult::MissingProtocol
            }
            Err(PrimarySelectionCheckError::NoSeats) => DataControlSemanticResult::NoSeats,
            Err(
                PrimarySelectionCheckError::SocketOpenError(_)
                | PrimarySelectionCheckError::WaylandConnection(_)
                | PrimarySelectionCheckError::WaylandCommunication(_),
            ) => DataControlSemanticResult::ConnectionUnavailable,
        };
        classify_data_control_semantic(semantic)
    }

    #[cfg(target_os = "linux")]
    fn env_value_present(value: Option<&std::ffi::OsStr>) -> bool {
        value.is_some_and(|value| !value.is_empty())
    }

    #[cfg(target_os = "linux")]
    fn env_present(var: &str) -> bool {
        env_value_present(std::env::var_os(var).as_deref())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn x11_display_env_present() -> bool {
        env_present("DISPLAY")
    }

    /// Deadline for CLI clipboard writes (payload piped through the tool).
    #[cfg(target_os = "linux")]
    const CLI_WRITE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

    /// Deadline for CLI availability probes and write read-backs.
    #[cfg(target_os = "linux")]
    const CLI_PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

    /// Deadline for CLI content reads (wl-paste/xclip text and image
    /// fallbacks) — the same budget as the in-process `ARBOARD_READ_WAIT` so
    /// a paste gets equal time on either backend.
    #[cfg(target_os = "linux")]
    const CLI_READ_WAIT: std::time::Duration = ARBOARD_READ_WAIT;

    #[cfg(target_os = "linux")]
    fn tool_available(spec: &ToolSpec) -> bool {
        let mut cmd = Command::new(spec.write_text[0]);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        // Availability = the tool ran and exited in time (any exit status).
        #[allow(clippy::disallowed_methods)] // availability probe, waited on with a timeout
        let Ok(mut child) = cmd.spawn() else {
            return false;
        };
        super::wait_with_deadline(&mut child, CLI_PROBE_WAIT).is_ok()
    }

    /// Cache only successful discovery so transient misses are retried.
    #[cfg(target_os = "linux")]
    fn cache_successful_probe(
        cache: &std::sync::OnceLock<()>,
        probe: impl FnOnce() -> bool,
    ) -> bool {
        if cache.get().is_some() {
            return true;
        }
        if !probe() {
            return false;
        }
        let _ = cache.set(());
        true
    }

    #[cfg(target_os = "linux")]
    fn x11_primary_tool_available(spec: &ToolSpec) -> bool {
        static XCLIP_DISCOVERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        static XSEL_DISCOVERED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if std::ptr::eq(spec, &XCLIP_SPEC) {
            cache_successful_probe(&XCLIP_DISCOVERED, || tool_available(&XCLIP_SPEC))
        } else {
            debug_assert!(std::ptr::eq(spec, &XSEL_SPEC));
            cache_successful_probe(&XSEL_DISCOVERED, || tool_available(&XSEL_SPEC))
        }
    }

    #[cfg(target_os = "linux")]
    fn primary_arboard_fallback_allowed(
        display_env_present: bool,
        wayland_env_present: bool,
    ) -> bool {
        display_env_present && !wayland_env_present
    }

    /// Primary probe: first matching (env + tool on PATH) in wl-copy → xclip → xsel order.
    #[cfg(target_os = "linux")]
    fn probe_tool_spec() -> Option<&'static ToolSpec> {
        let specs: &[(&str, &ToolSpec)] = &[
            ("WAYLAND_DISPLAY", &WL_SPEC),
            ("DISPLAY", &XCLIP_SPEC),
            ("DISPLAY", &XSEL_SPEC),
        ];
        for &(env_var, spec) in specs {
            if env_present(env_var) && tool_available(spec) {
                return Some(spec);
            }
        }
        None
    }

    /// Every CLI backend we should fire on write (not just the primary probe).
    ///
    /// Probe/`native_tool_name()` stays single-winner for labels; writes fire
    /// every backend that is viable for the session so hybrid Wayland+X11
    /// desktops (KDE/XWayland, GNOME) populate both selections. Order is
    /// wl-copy then xclip then xsel; at most one X11 tool (xclip preferred).
    #[cfg(target_os = "linux")]
    fn linux_write_tool_specs() -> &'static [&'static ToolSpec] {
        static SPECS: std::sync::OnceLock<Vec<&'static ToolSpec>> = std::sync::OnceLock::new();
        SPECS
            .get_or_init(|| {
                collect_linux_write_specs(
                    env_present("WAYLAND_DISPLAY"),
                    env_present("DISPLAY"),
                    tool_available(&WL_SPEC),
                    tool_available(&XCLIP_SPEC),
                    tool_available(&XSEL_SPEC),
                )
            })
            .as_slice()
    }

    /// Pure write-target selection (unit-testable without subprocesses).
    #[cfg(target_os = "linux")]
    fn collect_linux_write_specs(
        wayland_env: bool,
        display_env: bool,
        wl_ok: bool,
        xclip_ok: bool,
        xsel_ok: bool,
    ) -> Vec<&'static ToolSpec> {
        let mut out: Vec<&'static ToolSpec> = Vec::new();
        if wayland_env && wl_ok {
            out.push(&WL_SPEC);
        }
        if display_env {
            if xclip_ok {
                out.push(&XCLIP_SPEC);
            } else if xsel_ok {
                out.push(&XSEL_SPEC);
            }
        }
        out
    }

    /// Pipe `data` into a CLI tool's stdin.
    #[cfg(target_os = "linux")]
    fn run_pipe_in(argv: &[&str], data: &[u8]) -> anyhow::Result<()> {
        let (bin, args) = argv.split_first().expect("argv non-empty");

        // Spooled stdin (`spool_for_stdin`), not a pipe: clipboard tools
        // (wl-copy/xclip) daemonize and read the payload after forking, racing
        // a pipe write and possibly leaving the selection empty; the daemon
        // keeps its fd to the unlinked temp file.
        let stdin = super::spool_for_stdin(data)?;

        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        #[allow(clippy::disallowed_methods)] // short-lived clipboard helper, waited on below
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn {bin}: {e}"))?;
        let status = super::wait_with_deadline(&mut child, CLI_WRITE_WAIT)?;
        if !status.success() {
            anyhow::bail!("{bin} exited with status {status}");
        }
        Ok(())
    }

    /// Run a CLI tool and capture its stdout, bounded by `deadline`
    /// (`CLI_PROBE_WAIT` for read-backs, `CLI_READ_WAIT` for content reads).
    #[cfg(target_os = "linux")]
    fn run_capture_out_with_status(
        argv: &[&str],
        deadline: std::time::Duration,
    ) -> anyhow::Result<(std::process::ExitStatus, Vec<u8>)> {
        let (bin, args) = argv.split_first().expect("argv non-empty");
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        #[allow(clippy::disallowed_methods)] // short-lived clipboard helper, waited on below
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to run {bin}: {e}"))?;
        // Drain stdout on a worker so the deadline wait can kill a hung tool
        // without deadlocking on a full pipe; the kill EOFs the pipe and the
        // reader exits on its own.
        let mut stdout = child.stdout.take().expect("stdout piped");
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf);
            buf
        });
        let status = super::wait_with_deadline(&mut child, deadline)?;
        let stdout = reader.join().unwrap_or_default();
        Ok((status, stdout))
    }

    /// Capture CLI stdout. Non-zero exit → empty bytes (typed MIME absent), not Err.
    /// Spawn/timeout failures still return Err. Prefer this over
    /// [`run_capture_out_checked`] for content reads where a missing type is
    /// normal (image-only board + text probe, or the reverse).
    #[cfg(target_os = "linux")]
    fn run_capture_out(argv: &[&str], deadline: std::time::Duration) -> anyhow::Result<Vec<u8>> {
        let (status, stdout) = run_capture_out_with_status(argv, deadline)?;
        if !status.success() {
            return Ok(Vec::new());
        }
        Ok(stdout)
    }

    /// Capture CLI stdout; non-zero exit is Err. Use for paths that must
    /// distinguish tool failure from emptiness (e.g. PRIMARY multi-tool scan).
    #[cfg(target_os = "linux")]
    fn run_capture_out_checked(
        argv: &[&str],
        deadline: std::time::Duration,
    ) -> anyhow::Result<Vec<u8>> {
        let (status, stdout) = run_capture_out_with_status(argv, deadline)?;
        if !status.success() {
            let bin = argv.first().copied().unwrap_or("clipboard tool");
            anyhow::bail!("{bin} exited with status {status}");
        }
        Ok(stdout)
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug, Eq, PartialEq)]
    enum PrimaryCliRead {
        Text(String),
        Empty,
        Failed,
    }

    #[cfg(target_os = "linux")]
    fn read_x11_primary_with_tools(
        display_env_present: bool,
        mut available: impl FnMut(&ToolSpec) -> bool,
        mut capture: impl FnMut(&ToolSpec, &[&str]) -> anyhow::Result<Vec<u8>>,
    ) -> PrimaryCliRead {
        if !display_env_present {
            return PrimaryCliRead::Failed;
        }
        for spec in [&XCLIP_SPEC, &XSEL_SPEC] {
            if !available(spec) {
                continue;
            }
            let argv = spec
                .read_primary
                .expect("X11 PRIMARY tool must define read argv");
            match capture(spec, argv) {
                Ok(bytes) if bytes.is_empty() => return PrimaryCliRead::Empty,
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => return PrimaryCliRead::Text(text),
                    Err(e) => {
                        tracing::debug!(tool = spec.name, "X11 PRIMARY text was not UTF-8: {e}")
                    }
                },
                Err(e) => {
                    tracing::debug!(tool = spec.name, "X11 PRIMARY CLI read failed: {e}")
                }
            }
        }
        PrimaryCliRead::Failed
    }

    /// True if a Wayland read-back exactly matches what we wrote. `wl-paste
    /// --no-newline` returns the selection verbatim (no appended newline), so an
    /// exact byte comparison is correct.
    #[cfg(target_os = "linux")]
    fn wayland_readback_matches(readback: &[u8], text: &str) -> bool {
        readback == text.as_bytes()
    }

    /// Confirm a Wayland write landed by reading the selection back: wl-copy
    /// daemonizes and can exit 0 even when the selection ends up empty, so its
    /// exit status alone is not trustworthy.
    ///
    /// A transient miss only under-reports success (never a false "Copied!"),
    /// but a single immediate read races wl-copy's daemonized claim of the
    /// selection, so callers retry via `readback_with_retry`.
    #[cfg(target_os = "linux")]
    fn wayland_write_verified(spec: &ToolSpec, text: &str) -> bool {
        match run_capture_out(spec.read_text, CLI_PROBE_WAIT) {
            Ok(bytes) if wayland_readback_matches(&bytes, text) => true,
            Ok(_) => {
                tracing::debug!(
                    "clipboard read-back mismatch ({spec_name})",
                    spec_name = spec.name
                );
                false
            }
            Err(e) => {
                tracing::debug!(
                    "clipboard read-back failed ({spec_name}): {e}",
                    spec_name = spec.name
                );
                false
            }
        }
    }

    #[cfg(target_os = "linux")]
    const READBACK_ATTEMPTS: usize = 3;
    #[cfg(target_os = "linux")]
    const READBACK_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

    /// Retry `attempt` up to `attempts` times, pausing between failures.
    /// Pure over the injected closure so the retry policy is unit-testable.
    #[cfg(target_os = "linux")]
    fn readback_with_retry(
        mut attempt: impl FnMut() -> bool,
        attempts: usize,
        pause: std::time::Duration,
    ) -> bool {
        for i in 0..attempts {
            if attempt() {
                return true;
            }
            if i + 1 < attempts {
                std::thread::sleep(pause);
            }
        }
        false
    }

    /// Whether a successful Wayland CLI write still needs the wl-paste
    /// read-back to count as verified. With data-control the arboard write is
    /// authoritative and already succeeded, so wl-copy fires purely for
    /// post-exit persistence and its result no longer gates success. Pure for
    /// unit tests.
    #[cfg(target_os = "linux")]
    fn wayland_readback_required(
        reads_wayland_selection: bool,
        data_control: bool,
        arboard_ok: bool,
    ) -> bool {
        reads_wayland_selection && !(data_control && arboard_ok)
    }

    // -- Public API ----------------------------------------------------------

    pub fn get_text() -> anyhow::Result<Option<String>> {
        let mut arboard_error = None;
        match arboard_get_text() {
            Ok(Some(text)) => return Ok(Some(text)),
            // Wayland-only: arboard's empty answer is not authoritative,
            // fall through to wl-paste (see `wayland_tool_selected`).
            #[cfg(target_os = "linux")]
            Ok(None) if wayland_tool_selected(linux_tool_spec()) => {}
            Ok(None) => return Ok(None),
            Err(e) => {
                tracing::debug!("arboard get_text failed: {e}");
                arboard_error = Some(e);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(spec) = linux_tool_spec() {
            // Not checked: `wl-paste -t text` exits non-zero on image-only boards.
            let bytes = run_capture_out(spec.read_text, CLI_READ_WAIT)?;
            return Ok(if bytes.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&bytes).into_owned())
            });
        }
        if let Some(error) = arboard_error {
            return Err(error);
        }
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    pub fn get_primary_text() -> anyhow::Result<Option<String>> {
        let display_env_present = x11_display_env_present();
        if !display_env_present {
            return Ok(None);
        }
        let wayland_env_present = env_present("WAYLAND_DISPLAY");

        match read_x11_primary_with_tools(
            display_env_present,
            x11_primary_tool_available,
            |_, argv| run_capture_out_checked(argv, CLI_READ_WAIT),
        ) {
            PrimaryCliRead::Text(text) => return Ok(Some(text)),
            PrimaryCliRead::Empty => return Ok(None),
            PrimaryCliRead::Failed => {}
        }

        if primary_arboard_fallback_allowed(display_env_present, wayland_env_present) {
            return arboard_get_primary_text();
        }
        Ok(None)
    }

    pub fn set_text_with_outcome(text: &str) -> super::NativeWriteOutcome {
        // Fire every viable native backend: arboard first (with data-control it
        // sets the Wayland selection focus-free and is authoritative; on X11 it
        // can return Ok(()) while GNOME/VTE/KDE paste still reads Wayland), then
        // the CLI tools (`wl-copy`/`xclip`/`xsel`) that match what users verify
        // manually — wl-copy after arboard, so its daemonized process ends up
        // owning the Wayland selection and post-exit paste keeps working.
        // Callers (e.g. the TUI) layer OSC 52 separately for SSH/tmux/terminal
        // passthrough.
        let mut outcome = super::NativeWriteOutcome {
            data_control: wayland_data_control_supported(),
            ..Default::default()
        };

        match arboard_set_text(text) {
            Ok(()) => outcome.arboard_ok = true,
            Err(e) => tracing::debug!("arboard set_text failed: {e}"),
        }

        #[cfg(target_os = "linux")]
        for spec in linux_write_tool_specs() {
            outcome.cli_tools_tried.push(spec.name);
            match run_pipe_in(spec.write_text, text.as_bytes()) {
                Ok(()) => {
                    // Only Wayland writes need read-back (X11 tools are covered
                    // by arboard reads), and only while the read-back still
                    // gates success (see `wayland_readback_required`). The
                    // bounded retry covers the race against wl-copy's
                    // daemonized claim of the selection. Accepted residual of
                    // the skip: wl-copy re-claims the selection after the good
                    // arboard write, so a wl-copy daemon dying between claim
                    // and serve clobbers it undetected — post-exit persistence
                    // is worth that rare window.
                    let needs_readback = wayland_readback_required(
                        spec.reads_wayland_selection,
                        outcome.data_control,
                        outcome.arboard_ok,
                    );
                    let verified = !needs_readback
                        || readback_with_retry(
                            || wayland_write_verified(spec, text),
                            READBACK_ATTEMPTS,
                            READBACK_PAUSE,
                        );
                    if verified {
                        outcome.cli_ok = true;
                        outcome.cli_ok_tools.push(spec.name);
                    }
                }
                Err(e) => tracing::debug!(
                    "CLI clipboard write failed ({spec_name}): {e}",
                    spec_name = spec.name
                ),
            }
        }

        outcome.any_ok = outcome.cli_ok || outcome.arboard_ok;
        outcome
    }

    pub fn get_image() -> anyhow::Result<Option<ImageData>> {
        let mut arboard_error = None;
        match arboard_get_image() {
            Ok(Some(image)) => return Ok(Some(image)),
            // Wayland-only: arboard's empty answer is not authoritative,
            // fall through to wl-paste (see `wayland_tool_selected`).
            #[cfg(target_os = "linux")]
            Ok(None) if wayland_tool_selected(linux_tool_spec()) => {}
            Ok(None) => return Ok(None),
            Err(e) => {
                tracing::debug!("arboard get_image failed: {e}");
                arboard_error = Some(e);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(spec) = linux_tool_spec()
            && let Some(argv) = spec.read_png
        {
            // Not checked: typed image MIME miss is Ok(None), not probe failure.
            let bytes = run_capture_out(argv, CLI_READ_WAIT)?;
            if !bytes.is_empty() {
                let mime = super::mime_from_bytes(&bytes);
                return Ok(Some(ImageData {
                    data: bytes,
                    mime_type: mime.to_owned(),
                }));
            }
            return Ok(None);
        }
        if let Some(error) = arboard_error {
            return Err(error);
        }
        Ok(None)
    }

    /// Unlike `set_text`/`get_text`/`get_image`, this skips arboard and goes
    /// straight to CLI tools — arboard has no `set_image_file` API, and
    /// re-decoding/re-encoding through its RGBA pixel interface is wasteful
    /// when the file is already in a usable format. Uses the same multi-backend
    /// write list as text (`linux_write_tool_specs`).
    pub fn set_image_file(path: &std::path::Path) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let data = std::fs::read(path)?;
            let mut any_ok = false;
            let mut last_err: Option<anyhow::Error> = None;
            for spec in linux_write_tool_specs() {
                let Some(argv) = spec.write_png else {
                    continue;
                };
                match run_pipe_in(argv, &data) {
                    Ok(()) => any_ok = true,
                    Err(e) => {
                        tracing::debug!(
                            "CLI image clipboard write failed ({spec_name}): {e}",
                            spec_name = spec.name
                        );
                        last_err = Some(e);
                    }
                }
            }
            if any_ok {
                return Ok(());
            }
            if let Some(e) = last_err {
                return Err(e);
            }
            anyhow::bail!("no CLI tool supports image clipboard writes");
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            anyhow::bail!("image clipboard not supported on this platform")
        }
    }

    /// Encode raw RGBA pixels into PNG bytes.
    pub(super) fn encode_rgba_to_png(
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> anyhow::Result<Vec<u8>> {
        use image::codecs::png::PngEncoder;
        use image::{ColorType, ImageEncoder};

        let expected_len = (width as usize) * (height as usize) * 4;
        if rgba.len() < expected_len {
            anyhow::bail!(
                "RGBA buffer too short: expected at least {} bytes for {}x{}, got {}",
                expected_len,
                width,
                height,
                rgba.len()
            );
        }

        let mut png_buf = Vec::with_capacity(expected_len / 4);
        let encoder = PngEncoder::new(&mut png_buf);
        encoder.write_image(rgba, width, height, ColorType::Rgba8.into())?;
        Ok(png_buf)
    }

    pub fn get_attachments() -> anyhow::Result<super::ClipboardAttachments> {
        // `ContentNotAvailable` is already Ok(None); other file_list errors must
        // not skip get_image when a raster is still present.
        let file_urls = match get_file_urls() {
            Ok(urls) => urls,
            Err(e) => {
                tracing::debug!("clipboard file_urls read failed: {e}");
                None
            }
        };
        let image = if file_urls.is_none() {
            get_image()?
        } else {
            None
        };
        Ok(super::ClipboardAttachments { file_urls, image })
    }

    /// Read file references via arboard's `file_list()` — `CF_HDROP`
    /// on Windows, `text/uri-list` on X11. Returns newline-joined
    /// absolute paths.
    pub fn get_file_urls() -> anyhow::Result<Option<String>> {
        arboard_read_with_deadline(|clipboard| {
            let paths = match clipboard.get().file_list() {
                Ok(p) => p,
                Err(arboard::Error::ContentNotAvailable) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            if paths.is_empty() {
                return Ok(None);
            }
            Ok(Some(
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
        })
    }

    /// Contract tests for `spawn_with_deadline` (private to this module, so
    /// they live here; any non-macOS test host runs them).
    #[cfg(test)]
    mod worker_deadline_tests {
        use super::*;
        use std::sync::mpsc::RecvTimeoutError;
        use std::time::Duration;

        #[test]
        fn fast_closure_returns_value() {
            let result = spawn_with_deadline("test-fast", Duration::from_secs(5), || 42u8);
            assert_eq!(result, Ok(42));
        }

        /// Deadline expiry abandons the worker promptly instead of waiting it out.
        #[test]
        fn slow_closure_times_out() {
            let started = std::time::Instant::now();
            let result = spawn_with_deadline("test-slow", Duration::from_millis(30), || {
                std::thread::sleep(Duration::from_secs(5));
                0u8
            });
            assert_eq!(result, Err(RecvTimeoutError::Timeout));
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        /// A worker that dies before sending is distinguishable from a timeout
        /// (callers label it "worker died" rather than "timed out").
        #[test]
        fn panicking_closure_reports_disconnected() {
            let result = spawn_with_deadline("test-panic", Duration::from_secs(5), || -> u8 {
                panic!("worker panic (expected by this test)")
            });
            assert_eq!(result, Err(RecvTimeoutError::Disconnected));
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    mod linux_tests {
        use super::*;
        use crate::clipboard::WaylandDataControlProbe;

        #[test]
        fn primary_read_argv_targets_x11_primary_exactly() {
            assert_eq!(
                XCLIP_SPEC.read_primary,
                Some(&["xclip", "-o", "-selection", "primary"][..])
            );
            assert_eq!(
                XSEL_SPEC.read_primary,
                Some(&["xsel", "--primary", "--output"][..])
            );
            assert_eq!(WL_SPEC.read_primary, None);
        }

        #[test]
        fn primary_tool_selection_requires_nonempty_display_before_probing() {
            let calls = std::cell::RefCell::new(Vec::new());
            let read = read_x11_primary_with_tools(
                false,
                |spec| {
                    calls.borrow_mut().push(format!("discover {}", spec.name));
                    true
                },
                |spec, _| {
                    calls.borrow_mut().push(format!("read {}", spec.name));
                    Ok(b"text".to_vec())
                },
            );

            assert_eq!(read, PrimaryCliRead::Failed);
            assert!(
                calls.borrow().is_empty(),
                "non-X11 sessions must not invoke X11 tools"
            );
        }

        #[test]
        fn successful_xclip_never_discovers_xsel() {
            let calls = std::cell::RefCell::new(Vec::new());
            let read = read_x11_primary_with_tools(
                true,
                |spec| {
                    calls.borrow_mut().push(format!("discover {}", spec.name));
                    true
                },
                |spec, _| {
                    calls.borrow_mut().push(format!("read {}", spec.name));
                    Ok(b"from xclip".to_vec())
                },
            );

            assert_eq!(read, PrimaryCliRead::Text("from xclip".to_owned()));
            assert_eq!(calls.into_inner(), vec!["discover xclip", "read xclip"]);
        }

        #[test]
        fn primary_tool_discovery_caches_only_positive_results() {
            let cache = std::sync::OnceLock::new();
            let mut probes = 0;
            assert!(!cache_successful_probe(&cache, || {
                probes += 1;
                false
            }));
            assert!(cache_successful_probe(&cache, || {
                probes += 1;
                true
            }));
            assert!(cache_successful_probe(&cache, || {
                probes += 1;
                false
            }));
            assert_eq!(probes, 2, "the positive result must skip later probes");
        }

        #[test]
        fn primary_read_tries_xsel_after_xclip_runtime_failure() {
            let calls = std::cell::RefCell::new(Vec::new());
            let read = read_x11_primary_with_tools(
                true,
                |spec| {
                    calls.borrow_mut().push(format!("discover {}", spec.name));
                    true
                },
                |spec, _| {
                    calls.borrow_mut().push(format!("read {}", spec.name));
                    if spec.name == "xclip" {
                        anyhow::bail!("xclip runtime failure");
                    }
                    Ok(b"from xsel".to_vec())
                },
            );

            assert_eq!(read, PrimaryCliRead::Text("from xsel".to_owned()));
            assert_eq!(
                calls.into_inner(),
                vec!["discover xclip", "read xclip", "discover xsel", "read xsel"]
            );
        }

        #[test]
        fn primary_read_treats_successful_empty_as_authoritative() {
            let calls = std::cell::RefCell::new(Vec::new());
            let read = read_x11_primary_with_tools(
                true,
                |spec| {
                    calls.borrow_mut().push(format!("discover {}", spec.name));
                    true
                },
                |spec, _| {
                    calls.borrow_mut().push(format!("read {}", spec.name));
                    Ok(Vec::new())
                },
            );

            assert_eq!(read, PrimaryCliRead::Empty);
            assert_eq!(calls.into_inner(), vec!["discover xclip", "read xclip"]);
        }

        #[test]
        fn absent_xclip_discovers_xsel_lazily() {
            let calls = std::cell::RefCell::new(Vec::new());
            let read = read_x11_primary_with_tools(
                true,
                |spec| {
                    calls.borrow_mut().push(format!("discover {}", spec.name));
                    spec.name == "xsel"
                },
                |spec, _| {
                    calls.borrow_mut().push(format!("read {}", spec.name));
                    Ok(b"from xsel".to_vec())
                },
            );

            assert_eq!(read, PrimaryCliRead::Text("from xsel".to_owned()));
            assert_eq!(
                calls.into_inner(),
                vec!["discover xclip", "discover xsel", "read xsel"]
            );
        }

        #[test]
        fn primary_read_reports_failure_after_all_backends_fail() {
            let mut calls = Vec::new();
            let read = read_x11_primary_with_tools(
                true,
                |_| true,
                |spec, _| {
                    calls.push(spec.name);
                    anyhow::bail!("runtime failure")
                },
            );

            assert_eq!(read, PrimaryCliRead::Failed);
            assert_eq!(calls, vec!["xclip", "xsel"]);
        }

        #[test]
        fn primary_arboard_fallback_is_x11_only() {
            assert!(primary_arboard_fallback_allowed(true, false));
            assert!(!primary_arboard_fallback_allowed(true, true));
            assert!(!primary_arboard_fallback_allowed(false, false));
            assert!(!primary_arboard_fallback_allowed(false, true));
        }

        #[test]
        fn display_guard_rejects_absent_and_empty_values() {
            use std::ffi::OsStr;
            assert!(!env_value_present(None));
            assert!(!env_value_present(Some(OsStr::new(""))));
            assert!(env_value_present(Some(OsStr::new(":99"))));
        }

        /// Only the Wayland tool may override arboard's `Ok(None)`.
        #[test]
        fn wayland_tool_selected_per_spec() {
            assert!(wayland_tool_selected(Some(&WL_SPEC)));
            assert!(!wayland_tool_selected(Some(&XCLIP_SPEC)));
            assert!(!wayland_tool_selected(Some(&XSEL_SPEC)));
            assert!(!wayland_tool_selected(None));
        }

        /// Typed text/image argv: missing MIME must not dump raw bytes (non-zero
        /// exit → empty via `run_capture_out`, not a hard probe failure).
        #[test]
        fn wl_paste_content_reads_are_typed() {
            assert!(WL_SPEC.read_text.contains(&"-t"));
            assert!(WL_SPEC.read_text.contains(&"text"));
            let png = WL_SPEC.read_png.expect("wl-paste image read");
            assert!(png.contains(&"-t"));
            assert!(png.contains(&"image/png"));
        }

        #[test]
        fn write_specs_wayland_and_xclip_both_when_available() {
            let specs = collect_linux_write_specs(true, true, true, true, true);
            let names: Vec<_> = specs.iter().map(|s| s.name).collect();
            assert_eq!(names, vec!["wl-copy", "xclip"]);
        }

        #[test]
        fn write_specs_xclip_only_when_wl_missing() {
            let specs = collect_linux_write_specs(true, true, false, true, false);
            let names: Vec<_> = specs.iter().map(|s| s.name).collect();
            assert_eq!(names, vec!["xclip"]);
        }

        #[test]
        fn write_specs_wl_only_on_wayland_without_display() {
            let specs = collect_linux_write_specs(true, false, true, true, false);
            let names: Vec<_> = specs.iter().map(|s| s.name).collect();
            assert_eq!(names, vec!["wl-copy"]);
        }

        #[test]
        fn write_specs_xsel_when_xclip_unavailable() {
            let specs = collect_linux_write_specs(false, true, false, false, true);
            let names: Vec<_> = specs.iter().map(|s| s.name).collect();
            assert_eq!(names, vec!["xsel"]);
        }

        #[test]
        fn write_specs_prefers_xclip_over_xsel() {
            let specs = collect_linux_write_specs(false, true, false, true, true);
            let names: Vec<_> = specs.iter().map(|s| s.name).collect();
            assert_eq!(names, vec!["xclip"]);
        }

        #[test]
        fn readback_exact_match() {
            assert!(wayland_readback_matches(b"hello", "hello"));
            // Verbatim trailing newline must verify (wl-paste --no-newline does not strip it).
            assert!(wayland_readback_matches(b"AB\n", "AB\n"));
        }

        #[test]
        fn readback_genuine_mismatch() {
            assert!(!wayland_readback_matches(b"hello", "goodbye"));
        }

        #[test]
        fn readback_empty() {
            assert!(wayland_readback_matches(b"", ""));
        }

        /// The original bug: an empty selection (wl-copy exited 0 but stored nothing)
        /// must fail verification — empty read-back never matches non-empty text.
        #[test]
        fn readback_empty_selection_rejected() {
            assert!(!wayland_readback_matches(b"", "hello"));
            assert!(!wayland_readback_matches(b"", "hello\n"));
        }

        #[test]
        fn retry_succeeds_on_first_attempt_without_retrying() {
            let mut calls = 0;
            let ok = readback_with_retry(
                || {
                    calls += 1;
                    true
                },
                3,
                std::time::Duration::ZERO,
            );
            assert!(ok);
            assert_eq!(calls, 1);
        }

        /// The race this retry exists for: the first read lands before wl-copy's
        /// daemonized claim, a later one after.
        #[test]
        fn retry_recovers_from_transient_miss() {
            let mut calls = 0;
            let ok = readback_with_retry(
                || {
                    calls += 1;
                    calls == 3
                },
                3,
                std::time::Duration::ZERO,
            );
            assert!(ok);
            assert_eq!(calls, 3);
        }

        #[test]
        fn retry_bounded_on_persistent_failure() {
            let mut calls = 0;
            let ok = readback_with_retry(
                || {
                    calls += 1;
                    false
                },
                3,
                std::time::Duration::ZERO,
            );
            assert!(!ok);
            assert_eq!(calls, 3);
        }

        /// Read-back gating, exhaustive: skipped only when data-control made
        /// the arboard write authoritative; X11 tools never need it.
        #[test]
        fn readback_required_matrix() {
            assert!(!wayland_readback_required(true, true, true));
            assert!(wayland_readback_required(true, true, false));
            assert!(wayland_readback_required(true, false, true));
            assert!(wayland_readback_required(true, false, false));
            assert!(!wayland_readback_required(false, true, true));
            assert!(!wayland_readback_required(false, true, false));
            assert!(!wayland_readback_required(false, false, true));
            assert!(!wayland_readback_required(false, false, false));
        }

        #[test]
        fn data_control_semantic_classification_is_exhaustive() {
            assert_eq!(
                classify_data_control_semantic(DataControlSemanticResult::Present),
                WaylandDataControlProbe::Available(true)
            );
            assert_eq!(
                classify_data_control_semantic(DataControlSemanticResult::MissingProtocol),
                WaylandDataControlProbe::Available(false)
            );
            assert_eq!(
                classify_data_control_semantic(DataControlSemanticResult::NoSeats),
                WaylandDataControlProbe::Unavailable
            );
            assert_eq!(
                classify_data_control_semantic(DataControlSemanticResult::ConnectionUnavailable),
                WaylandDataControlProbe::Unavailable
            );
        }

        #[test]
        fn dependency_error_mapping_pins_directly_constructible_variants() {
            use wl_clipboard_rs::utils::PrimarySelectionCheckError;
            assert_eq!(
                data_control_from_result(Ok(true)),
                WaylandDataControlProbe::Available(true)
            );
            assert_eq!(
                data_control_from_result(Ok(false)),
                WaylandDataControlProbe::Available(true)
            );
            assert_eq!(
                data_control_from_result(Err(PrimarySelectionCheckError::MissingProtocol)),
                WaylandDataControlProbe::Available(false)
            );
            assert_eq!(
                data_control_from_result(Err(PrimarySelectionCheckError::NoSeats)),
                WaylandDataControlProbe::Unavailable
            );
            assert_eq!(
                data_control_from_result(Err(PrimarySelectionCheckError::SocketOpenError(
                    std::io::Error::other("boom")
                ))),
                WaylandDataControlProbe::Unavailable
            );
        }

        /// The permanence bug this policy prevents: an unanswered probe must
        /// not decide `false` (the equal-deadline lease init may succeed via
        /// data-control moments later) until the retry cap is exhausted.
        #[test]
        fn probe_cache_indefinite_stays_undecided_below_cap() {
            let mut cache = ProbeCache::new();
            for seen in 1..PROBE_INDEFINITE_MAX {
                assert!(!apply_probe_outcome(
                    &mut cache,
                    &WaylandDataControlProbe::Unavailable
                ));
                assert_eq!(cache.decided, None);
                assert_eq!(cache.indefinite_seen, seen);
            }
            // Cap reached: permanently decided false (mirrors lease degradation).
            assert!(!apply_probe_outcome(
                &mut cache,
                &WaylandDataControlProbe::Unavailable
            ));
            assert_eq!(cache.decided, Some(false));
        }

        /// A definitive answer after transient failures decides truth exactly
        /// once; the earlier indefinite attempts leave no residue in the
        /// decision.
        #[test]
        fn repeated_probe_errors_reach_permanent_false() {
            let mut cache = ProbeCache::new();
            for seen in 1..PROBE_INDEFINITE_MAX {
                assert!(!apply_probe_outcome(
                    &mut cache,
                    &WaylandDataControlProbe::Error("worker died".to_owned())
                ));
                assert_eq!(cache.decided, None);
                assert_eq!(cache.indefinite_seen, seen);
            }
            assert!(!apply_probe_outcome(
                &mut cache,
                &WaylandDataControlProbe::Error("worker died".to_owned())
            ));
            assert_eq!(cache.decided, Some(false));
        }

        #[test]
        fn probe_cache_definitive_after_indefinite_decides_truth() {
            let mut cache = ProbeCache::new();
            assert!(!apply_probe_outcome(
                &mut cache,
                &WaylandDataControlProbe::Unavailable
            ));
            assert!(apply_probe_outcome(
                &mut cache,
                &WaylandDataControlProbe::Available(true)
            ));
            assert_eq!(cache.decided, Some(true));
            assert_eq!(cache.indefinite_seen, 1);
        }

        #[test]
        fn probe_cache_definitive_false_decides_immediately() {
            let mut cache = ProbeCache::new();
            assert!(!apply_probe_outcome(
                &mut cache,
                &WaylandDataControlProbe::Available(false)
            ));
            assert_eq!(cache.decided, Some(false));
        }

        /// Once decided, callers take the fast path: the probe never runs
        /// again and a later (contradictory) probe cannot flip the answer.
        #[test]
        fn decided_cache_skips_further_probes() {
            let cache = parking_lot::Mutex::new(ProbeCache::new());
            let calls = std::cell::Cell::new(0u32);
            assert!(cached_probe_answer(&cache, || {
                calls.set(calls.get() + 1);
                WaylandDataControlProbe::Available(true)
            }));
            assert!(cached_probe_answer(&cache, || {
                calls.set(calls.get() + 1);
                WaylandDataControlProbe::Available(false)
            }));
            assert_eq!(calls.get(), 1);
        }

        /// Concurrent callers serialize on the cache lock: one probe
        /// execution, one shared answer — a burst can't stack indefinite
        /// outcomes past the cap or drop a definitive answer.
        #[test]
        fn concurrent_callers_share_one_probe() {
            use std::sync::atomic::{AtomicU32, Ordering};
            let cache = parking_lot::Mutex::new(ProbeCache::new());
            let calls = AtomicU32::new(0);
            std::thread::scope(|s| {
                for _ in 0..4 {
                    s.spawn(|| {
                        let answer = cached_probe_answer(&cache, || {
                            calls.fetch_add(1, Ordering::Relaxed);
                            // Hold the lock long enough that peers must wait.
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            WaylandDataControlProbe::Available(true)
                        });
                        assert!(answer);
                    });
                }
            });
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }

        /// The probe and the lease init connect on one shared budget, so
        /// "probe timed out but init succeeded" can't come from skewed
        /// deadlines.
        #[test]
        fn probe_and_lease_share_connect_deadline() {
            assert_eq!(DISPLAY_CONN_WAIT, std::time::Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    mod mac_command_status {
        use std::os::unix::process::ExitStatusExt;

        fn output(code: i32, stdout: &[u8]) -> std::process::Output {
            std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.to_vec(),
                stderr: b"backend failed".to_vec(),
            }
        }

        #[test]
        fn exit_zero_empty_is_successful_emptiness() {
            assert_eq!(
                super::super::platform::checked_command_stdout("pbpaste", Ok(output(0, b"")))
                    .expect("exit-zero empty output"),
                Vec::<u8>::new()
            );
        }

        #[test]
        fn exit_zero_output_is_preserved() {
            assert_eq!(
                super::super::platform::checked_command_stdout(
                    "pbpaste",
                    Ok(output(0, b"exact\n"))
                )
                .expect("exit-zero output"),
                b"exact\n"
            );
        }

        #[test]
        fn nonzero_and_spawn_failure_are_errors() {
            assert!(
                super::super::platform::checked_command_stdout(
                    "pbpaste",
                    Ok(output(1, b"ignored"))
                )
                .is_err()
            );
            assert!(
                super::super::platform::checked_command_stdout(
                    "osascript",
                    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
                )
                .is_err()
            );
        }
    }

    /// Round-trip: write then read back.
    ///
    /// This test is ignored by default because it mutates the real system
    /// clipboard, which is undesirable in CI and may interfere with the
    /// user's clipboard contents. Run with `cargo test -- --ignored` locally.
    #[test]
    #[ignore]
    fn round_trip() {
        let sentinel = format!("echo-agent-clipboard-test-{}", std::process::id());
        set_text(&sentinel).expect("set_text failed");
        let got = get_text().expect("get_text failed");
        assert_eq!(got.as_deref(), Some(sentinel.as_str()));
    }

    // -----------------------------------------------------------------------
    // wait_with_deadline (real child processes; unix `sleep`)
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
    fn spawn_sleep(seconds: &str) -> std::process::Child {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg(seconds)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        echo_agent_tools::util::detach_std_command(&mut cmd);
        cmd.spawn().expect("spawn sleep")
    }

    #[cfg(unix)]
    #[test]
    fn wait_with_deadline_returns_status_of_fast_child() {
        let mut child = spawn_sleep("0");
        let status = wait_with_deadline(&mut child, std::time::Duration::from_secs(5))
            .expect("fast child must be reaped in time");
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    fn wait_with_deadline_kills_hung_child_and_reports_timeout() {
        let mut child = spawn_sleep("30");
        let started = std::time::Instant::now();
        let err = wait_with_deadline(&mut child, std::time::Duration::from_millis(50))
            .expect_err("deadline must expire");
        assert!(
            err.downcast_ref::<WaitTimeout>().is_some(),
            "timeout must be distinguishable, got: {err}"
        );
        // Killed and reaped, not waited for the full 30 s.
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(child.try_wait().expect("child reaped").is_some());
    }

    // -----------------------------------------------------------------------
    // spool_for_stdin (unlink-then-read contract)
    // -----------------------------------------------------------------------

    /// The two contracts callers rely on: the returned fd stays readable after
    /// the temp file is unlinked on return, and a payload well past the
    /// ~64 KiB pipe buffer (the reason the spool exists) round-trips intact.
    #[test]
    fn spool_for_stdin_round_trips_large_payload_after_unlink() {
        use std::io::Read;
        let payload: Vec<u8> = (0..1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let mut file = spool_for_stdin(&payload).expect("spool payload");
        let mut read_back = Vec::new();
        file.read_to_end(&mut read_back).expect("read spooled file");
        assert_eq!(read_back, payload);
    }

    // -----------------------------------------------------------------------
    // OSC 52 sequence construction (pure; base64("hi") == "aGk=")
    // -----------------------------------------------------------------------

    #[test]
    fn osc52_sequence_plain() {
        assert_eq!(osc52_sequence("hi", false), b"\x1b]52;c;aGk=\x07".to_vec());
    }

    #[test]
    fn osc52_sequence_tmux_passthrough() {
        assert_eq!(
            osc52_sequence("hi", true),
            b"\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\".to_vec()
        );
    }

    // -----------------------------------------------------------------------
    // MIME detection from magic bytes
    // -----------------------------------------------------------------------

    #[test]
    fn mime_from_bytes_png() {
        let header = b"\x89PNG\r\n\x1a\nrest";
        assert_eq!(mime_from_bytes(header), "image/png");
    }

    #[test]
    fn mime_from_bytes_jpeg() {
        let header = b"\xff\xd8\xffrest";
        assert_eq!(mime_from_bytes(header), "image/jpeg");
    }

    #[test]
    fn mime_from_bytes_tiff_le() {
        let header = b"II\x2a\x00rest";
        assert_eq!(mime_from_bytes(header), "image/tiff");
    }

    #[test]
    fn mime_from_bytes_tiff_be() {
        let header = b"MM\x00\x2arest";
        assert_eq!(mime_from_bytes(header), "image/tiff");
    }

    #[test]
    fn mime_from_bytes_gif() {
        assert_eq!(mime_from_bytes(b"GIF89a"), "image/gif");
        assert_eq!(mime_from_bytes(b"GIF87a"), "image/gif");
    }

    #[test]
    fn mime_from_bytes_webp() {
        let header = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(mime_from_bytes(header), "image/webp");
    }

    #[test]
    fn mime_from_bytes_bmp() {
        assert_eq!(mime_from_bytes(b"BM\x00\x00"), "image/bmp");
    }

    #[test]
    fn mime_from_bytes_unknown() {
        assert_eq!(mime_from_bytes(b""), "application/octet-stream");
        assert_eq!(mime_from_bytes(b"\x00\x01"), "application/octet-stream");
    }

    // -----------------------------------------------------------------------
    // MIME to extension mapping
    // -----------------------------------------------------------------------

    #[test]
    fn mime_to_extension_known() {
        assert_eq!(mime_to_extension("image/png"), "png");
        assert_eq!(mime_to_extension("image/jpeg"), "jpg");
        assert_eq!(mime_to_extension("image/tiff"), "tiff");
        assert_eq!(mime_to_extension("image/gif"), "gif");
        assert_eq!(mime_to_extension("image/webp"), "webp");
        assert_eq!(mime_to_extension("image/bmp"), "bmp");
    }

    #[test]
    fn mime_to_extension_unknown() {
        assert_eq!(mime_to_extension("application/octet-stream"), "bin");
        assert_eq!(mime_to_extension("text/plain"), "bin");
    }

    // -----------------------------------------------------------------------
    // Linux RGBA-to-PNG encoding (only compiled on non-macOS)
    // -----------------------------------------------------------------------

    #[cfg(not(target_os = "macos"))]
    mod linux_encoding {
        use super::super::platform::encode_rgba_to_png;
        use super::*;

        #[test]
        fn encode_1x1_red_pixel() {
            // 1x1 RGBA: fully opaque red
            let rgba = [255u8, 0, 0, 255];
            let png = encode_rgba_to_png(&rgba, 1, 1).expect("encoding failed");
            // Verify it starts with PNG magic
            assert_eq!(mime_from_bytes(&png), "image/png");
            assert!(png.len() > 8, "PNG output too short");
        }

        #[test]
        fn encode_2x2_pixels() {
            // 2x2 RGBA: 16 bytes
            let rgba = [0u8; 16];
            let png = encode_rgba_to_png(&rgba, 2, 2).expect("encoding failed");
            assert_eq!(mime_from_bytes(&png), "image/png");
        }

        #[test]
        fn encode_buffer_too_short() {
            // 2x2 needs 16 bytes, provide only 8
            let rgba = [0u8; 8];
            let result = encode_rgba_to_png(&rgba, 2, 2);
            assert!(result.is_err());
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("RGBA buffer too short"),
                "unexpected error: {msg}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // macOS unified attachments osascript stdout protocol (pure parsing)
    // -----------------------------------------------------------------------

    mod attachments_protocol_tests {
        use super::super::attachments_protocol::{
            FURL_MARKER, IMAGE_MARKER, parse_attachments_output, parse_osascript_furl_output,
        };

        fn sample_output(furl_body: &str, image: &str) -> String {
            format!("{FURL_MARKER}\n{furl_body}\n{IMAGE_MARKER}\nIMAGE:{image}")
        }

        // -----------------------------------------------------------
        // parse_osascript_furl_output: deterministic parsing surface
        // -----------------------------------------------------------

        #[test]
        fn parse_furl_empty_inputs_are_none() {
            for raw in ["", "\n", "   \n\t  ", "\r\n", "   \n  \t  \n"] {
                assert!(
                    parse_osascript_furl_output(raw).is_none(),
                    "expected None for {raw:?}"
                );
            }
        }

        #[test]
        fn parse_furl_none_sentinel_is_none() {
            for raw in ["none", "none\n", "  none  ", "none\r\n"] {
                assert!(
                    parse_osascript_furl_output(raw).is_none(),
                    "expected None for {raw:?}"
                );
            }
        }

        #[test]
        fn parse_furl_single_path_trailing_newline() {
            assert_eq!(
                parse_osascript_furl_output("/tmp/foo.txt\n"),
                Some("/tmp/foo.txt".to_owned()),
            );
        }

        #[test]
        fn parse_furl_multi_path_trailing_newline() {
            // The AppleScript list path appends `\n` after every
            // entry, so multi-file output ends with a trailing
            // newline that `trim` strips. Internal newlines are
            // preserved verbatim — the drop-path parser splits on
            // them.
            assert_eq!(
                parse_osascript_furl_output("/a\n/b\n"),
                Some("/a\n/b".to_owned()),
            );
        }

        #[test]
        fn parse_furl_multi_path_no_trailing_newline() {
            assert_eq!(
                parse_osascript_furl_output("/a\n/b"),
                Some("/a\n/b".to_owned()),
            );
        }

        // -----------------------------------------------------------
        // parse_attachments_output: unified osascript stdout protocol
        // -----------------------------------------------------------

        #[test]
        fn parse_attachments_none_none() {
            let (urls, class) = parse_attachments_output(&sample_output("none", "NONE"));
            assert!(urls.is_none());
            assert!(class.is_none());
        }

        #[test]
        fn parse_attachments_furl_only_skips_image_probe() {
            let raw = sample_output("/tmp/a.png\n", "NONE");
            let (urls, class) = parse_attachments_output(&raw);
            assert_eq!(urls.as_deref(), Some("/tmp/a.png"));
            assert!(class.is_none());
        }

        #[test]
        fn parse_attachments_multi_furl() {
            let raw = sample_output("/a\n/b\n", "NONE");
            let (urls, class) = parse_attachments_output(&raw);
            assert_eq!(urls.as_deref(), Some("/a\n/b"));
            assert!(class.is_none());
        }

        #[test]
        fn parse_attachments_image_classes() {
            for (image, expected) in [
                ("PNGf", Some("PNGf")),
                ("TIFF", Some("TIFF")),
                ("JPEG", Some("JPEG")),
            ] {
                let raw = sample_output("none", image);
                let (urls, class) = parse_attachments_output(&raw);
                assert!(urls.is_none(), "furl should be none for {image}");
                assert_eq!(class, expected);
            }
        }

        #[test]
        fn parse_attachments_unknown_image_class_is_none() {
            for image in ["WEBP", "GIFf", "unknown"] {
                let raw = sample_output("none", image);
                let (urls, class) = parse_attachments_output(&raw);
                assert!(urls.is_none(), "furl should be none for {image}");
                assert!(class.is_none(), "unknown class {image:?} should not parse");
            }
        }

        #[test]
        fn parse_attachments_furl_and_pngf_combo() {
            let raw = sample_output("/tmp/a.png\n", "PNGf");
            let (urls, class) = parse_attachments_output(&raw);
            assert_eq!(urls.as_deref(), Some("/tmp/a.png"));
            assert_eq!(class, Some("PNGf"));
        }

        #[test]
        fn parse_attachments_empty_stdout() {
            assert_eq!(parse_attachments_output(""), (None, None));
            assert_eq!(parse_attachments_output("   \n\t  "), (None, None));
        }

        #[test]
        fn parse_attachments_furl_only_without_image_marker() {
            let raw = format!("{FURL_MARKER}\n/a\n/b\n");
            let (urls, class) = parse_attachments_output(&raw);
            assert_eq!(urls.as_deref(), Some("/a\n/b"));
            assert!(class.is_none());
        }

        #[test]
        fn parse_attachments_furl_and_jpeg() {
            let raw = sample_output("none", "JPEG");
            let (urls, class) = parse_attachments_output(&raw);
            assert!(urls.is_none());
            assert_eq!(class, Some("JPEG"));
        }

        #[test]
        fn parse_attachments_crlf_markers() {
            let raw = format!("{FURL_MARKER}\r\n/tmp/a.png\r\n{IMAGE_MARKER}\r\nIMAGE:PNGf");
            let (urls, class) = parse_attachments_output(&raw);
            assert_eq!(urls.as_deref(), Some("/tmp/a.png"));
            assert_eq!(class, Some("PNGf"));
        }

        #[test]
        fn parse_attachments_whitespace_only_furl_body() {
            let (urls, class) = parse_attachments_output(&sample_output("   \n  \t  ", "NONE"));
            assert!(urls.is_none());
            assert!(class.is_none());
        }

        #[test]
        fn parse_attachments_malformed_returns_none() {
            assert_eq!(parse_attachments_output("garbage"), (None, None));
            // FURL section without IMAGE marker still yields paths.
            assert_eq!(
                parse_attachments_output("<<<FURL>>>\n/tmp/foo.txt"),
                (Some("/tmp/foo.txt".to_owned()), None),
            );
        }
    }

    // -----------------------------------------------------------------------
    // macOS extension helper
    // -----------------------------------------------------------------------

    #[cfg(target_os = "macos")]
    mod macos_helpers {
        use super::super::platform::extension_for_class;

        #[test]
        fn extension_mapping() {
            assert_eq!(extension_for_class("PNGf"), "png");
            assert_eq!(extension_for_class("TIFF"), "tiff");
            assert_eq!(extension_for_class("JPEG"), "jpg");
            assert_eq!(extension_for_class("unknown"), "bin");
        }
    }

    // -----------------------------------------------------------------------
    // get_image returns Ok(None) when no image is on the clipboard
    // -----------------------------------------------------------------------

    #[test]
    #[ignore] // requires real clipboard access
    fn get_image_text_only_clipboard() {
        // Put text on the clipboard, then check that get_image returns None.
        set_text("just text").expect("set_text failed");
        let result = get_image().expect("get_image failed");
        assert!(
            result.is_none(),
            "expected None when clipboard has text only"
        );
    }

    // -----------------------------------------------------------------------
    // Fast-probe type-list classification (clipboard_image_snapshot)
    // -----------------------------------------------------------------------

    fn types<'a>(list: &'a [&'static [u8]]) -> impl Iterator<Item = &'static [u8]> + 'a {
        list.iter().copied()
    }

    /// A screenshot/image copy advertises raster types with no file URLs:
    /// pasteable.
    #[test]
    fn image_copy_is_pasteable() {
        assert!(image_pasteable_from_types(types(&[
            b"public.png",
            b"public.tiff"
        ])));
        assert!(image_pasteable_from_types(types(&[b"public.jpeg"])));
    }

    /// A Finder file copy advertises a file-icon raster ALONGSIDE file URLs;
    /// paste routes those through path handling («class furl» wins in
    /// `get_attachments`), so the board does NOT hold a pasteable image —
    /// regardless of type order.
    #[test]
    fn finder_file_copy_is_not_pasteable_image() {
        assert!(!image_pasteable_from_types(types(&[
            b"public.file-url",
            b"public.tiff",
        ])));
        assert!(!image_pasteable_from_types(types(&[
            b"public.tiff",
            b"public.file-url",
        ])));
        // Pre-UTI spelling of the same advertisement.
        assert!(!image_pasteable_from_types(types(&[
            b"public.png",
            b"NSFilenamesPboardType",
        ])));
    }

    /// No raster types at all (plain text, etc.): not an image.
    #[test]
    fn text_only_board_is_not_image() {
        assert!(!image_pasteable_from_types(types(&[
            b"public.utf8-plain-text"
        ])));
        assert!(!image_pasteable_from_types(types(&[])));
    }

    // -----------------------------------------------------------------------
    // Native paste-time read type selection (native_image_type_from_types)
    // -----------------------------------------------------------------------

    /// The native read requests raster types in the osascript coercion
    /// order — PNG first, TIFF, then JPEG — regardless of advertised order.
    #[test]
    fn native_type_priority_matches_osascript_order() {
        assert_eq!(
            native_image_type_from_types(&[b"public.tiff".as_slice(), b"public.png"]),
            Some((b"public.png".as_slice(), "image/png")),
        );
        assert_eq!(
            native_image_type_from_types(&[b"public.jpeg".as_slice(), b"public.tiff"]),
            Some((b"public.tiff".as_slice(), "image/tiff")),
        );
        assert_eq!(
            native_image_type_from_types(&[b"public.jpeg".as_slice()]),
            Some((b"public.jpeg".as_slice(), "image/jpeg")),
        );
    }

    /// File-URL advertisements force the osascript furl path (`None`):
    /// the native read must never swallow a Finder file copy as raster.
    #[test]
    fn native_type_defers_to_furl_path() {
        assert_eq!(
            native_image_type_from_types(&[b"public.file-url".as_slice(), b"public.png"]),
            None,
        );
        assert_eq!(
            native_image_type_from_types(&[b"public.png".as_slice(), b"NSFilenamesPboardType"]),
            None,
        );
    }

    /// Raster-less boards (text, empty) never native-read.
    #[test]
    fn native_type_none_without_raster() {
        assert_eq!(
            native_image_type_from_types(&[b"public.utf8-plain-text".as_slice()]),
            None,
        );
        let empty: [&[u8]; 0] = [];
        assert_eq!(native_image_type_from_types(&empty), None);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_probe_smoke {
    use super::*;

    /// Manual smoke for the objc2 shim — nothing automated executes it (CI is
    /// Linux, the unit suites fake the probe). Run on a Mac:
    /// `cargo test -p echo-agent-shared -- --ignored probe_smoke`.
    #[test]
    #[ignore = "requires a real pasteboard; run manually on macOS"]
    fn probe_smoke_returns_some_on_macos() {
        // changeCount resolves on a real macOS session; the image bit depends
        // on whatever is currently on the pasteboard, so just exercise it.
        let (change_count, _has_image) = clipboard_image_snapshot();
        assert!(
            change_count.is_some(),
            "changeCount probe should resolve on a real macOS session"
        );
    }

    /// The native paste-time read must agree with the snapshot classification
    /// on whatever is currently on the pasteboard: snapshot says no pasteable
    /// raster ⇒ the native read returns None; snapshot says raster ⇒ the
    /// native read yields non-empty encoded bytes (both read the same
    /// advertised type list). Non-mutating — safe to run on a dev machine.
    #[test]
    #[ignore = "requires a real pasteboard; run manually on macOS"]
    fn probe_smoke_native_read_consistent_with_snapshot() {
        let (_change_count, has_image) = clipboard_image_snapshot();
        let native = platform::native_image_read();
        if has_image {
            let img = native.expect("snapshot reported a pasteable raster");
            assert!(!img.data.is_empty(), "native read returned empty bytes");
            assert!(img.mime_type.starts_with("image/"));
        } else {
            assert!(
                native.is_none(),
                "native read must not fire when the snapshot rules out a raster"
            );
        }
    }
}
