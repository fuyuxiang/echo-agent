//! Microphone capture (optional `audio` feature).
//!
//! Three backends share one interface (`spawn_pcm_capture`,
//! `capture_pcm_for_duration`, `input_device_info`, `CaptureHandle`):
//!
//! - **Linux**: a subprocess recorder (`pw-record`/`parec`/`arecord`) — the
//!   static-musl release binary cannot link `cpal` → `alsa-sys`; see
//!   [`capture_linux`].
//! - **macOS**: a subprocess too — the self-exec `__mic-capture` helper —
//!   because in-process CoreAudio memory is never returned after the stream
//!   drops; see [`capture_subprocess`].
//! - **Windows**: `cpal` (WASAPI) in-process; its memory cost is modest.
//!
//! The fixed-duration probe capture stays in-process on macOS/Windows: it only
//! runs in short-lived diagnostic processes, where the memory dies at exit.
//!
//! `CaptureHandle` is deliberately one name per platform, resolved by the
//! re-exports below:
//! - Linux → `pipe::ChildCaptureHandle` (recorder subprocess);
//! - macOS → `capture_subprocess::CaptureHandle`, an enum over the helper
//!   subprocess and the in-process fallback;
//! - Windows → `capture::CaptureHandle` (in-process cpal stream).

// cpal-based capture: the Windows backend, the macOS fallback, and the macOS
// `__mic-capture` child implementation.
#[cfg(not(target_os = "linux"))]
mod capture;
// Wire protocol shared by the `__mic-capture` child (writer, in `capture`)
// and the macOS parent (parser, in `capture_subprocess`).
#[cfg(not(target_os = "linux"))]
mod protocol;
#[cfg(not(target_os = "linux"))]
pub use capture::capture_pcm_for_duration;
#[cfg(not(target_os = "linux"))]
pub(crate) use capture::run_capture_child_cli;
#[cfg(target_os = "windows")]
pub use capture::{CaptureHandle, input_device_info, spawn_pcm_capture};

// Shared PCM-over-pipe plumbing for the two subprocess backends.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod pipe;

#[cfg(target_os = "macos")]
mod capture_subprocess;
#[cfg(target_os = "macos")]
pub use capture_subprocess::{CaptureHandle, input_device_info, spawn_pcm_capture};

#[cfg(target_os = "linux")]
mod capture_linux;
#[cfg(target_os = "linux")]
pub use capture_linux::{
    CaptureHandle, capture_pcm_for_duration, input_device_info, spawn_pcm_capture,
};
