//! Voice input for EchoAgent Build CLI: an EchoAgent streaming STT client and the
//! [`run_voice_pipeline`] task that emits [`VoiceEvent`]s for the pager.
//!
//! Voice is dictation only: mic → streaming STT → transcript into the prompt box.
//!
//! On macOS and Linux the microphone is opened in a short-lived subprocess so
//! the long-lived TUI never pays the platform audio stack's permanent memory
//! cost (see [`audio`] and [`maybe_run_capture_subprocess`]).

#[cfg(feature = "audio")]
pub mod audio;
pub mod auth;
pub mod config;
pub mod error;
pub mod event;
pub mod language;
pub mod pipeline;
pub mod probe;
pub mod stt;

pub use auth::{SharedVoiceAuth, StaticVoiceAuth, VoiceAuthProvider};
pub use config::VoiceConfig;
pub use error::VoiceError;
pub use event::VoiceEvent;
pub use language::{
    STT_LANGUAGE_AUTO, STT_LANGUAGE_DEFAULT, STT_LANGUAGES, SttLanguage, canonicalize_stt_language,
    language_for_api, stt_language_by_code,
};
pub use pipeline::{VoiceCommand, run_voice_pipeline};
#[cfg(feature = "audio")]
pub use probe::run_mic_only_probe;
pub use probe::{
    InputDeviceInfo, VoiceProbeOptions, VoiceProbeReport, format_probe_report, input_device_info,
    run_streaming_probe,
};

/// Whether this build can capture microphone audio (the `audio` feature).
/// Production CLI builds enable it on every OS: macOS/Windows link `cpal`
/// (coreaudio/wasapi), while Linux shells out to a system recorder
/// (`pw-record`/`parec`/`arecord`) so the static-musl binary links no audio
/// library. Bazel builds drop `audio` (no capture in the test sandbox).
///
/// On Linux a `true` value means capture is *compiled in*; whether a recorder
/// is actually installed is reported when a session starts. Consumers gate voice
/// on this so a no-audio build never advertises a mic it can't open.
pub const AUDIO_SUPPORTED: bool = cfg!(feature = "audio");

/// Hidden subcommand consumers re-exec themselves with to capture microphone
/// audio in a short-lived helper process (macOS; see
/// [`audio::capture_subprocess`](audio) for why capture is out of process).
/// Intercepted via [`maybe_run_capture_subprocess`] at the very top of `main`,
/// before any TUI/agent/tokio init, so the child stays minimal.
pub const MIC_CAPTURE_SUBCOMMAND: &str = "__mic-capture";

/// If this process was re-exec'd as the hidden mic-capture helper, run it and
/// return `Some(exit_code)`; otherwise `None` (a normal invocation). Call at
/// the very top of `main` in every binary that links this crate with `audio`
/// (the pager composition root and `voice-probe`), mirroring the pager's
/// mermaid render child intercept.
pub fn maybe_run_capture_subprocess() -> Option<i32> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if !is_capture_subcommand(&argv) {
        return None;
    }
    #[cfg(all(feature = "audio", not(target_os = "linux")))]
    {
        // Skip argv[0] (binary) and argv[1] (subcommand); the rest are flags.
        let args: Vec<String> = argv
            .into_iter()
            .skip(2)
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        Some(audio::run_capture_child_cli(args))
    }
    #[cfg(not(all(feature = "audio", not(target_os = "linux"))))]
    {
        // Never spawned by this build's own parent backend (Linux uses system
        // recorders; no-audio builds have no capture). Reachable only by hand.
        // `write!` not `println!`: never panic on a closed pipe.
        use std::io::Write;
        let _ = writeln!(
            std::io::stdout(),
            "ERR mic-capture helper unavailable in this build"
        );
        Some(2)
    }
}

/// Whether `argv` (the full process argv, incl. argv[0]) invokes the hidden
/// mic-capture helper — i.e. argv[1] is [`MIC_CAPTURE_SUBCOMMAND`]. Pure so the
/// dispatch decision is unit-testable without mutating the process's real args.
fn is_capture_subcommand(argv: &[std::ffi::OsString]) -> bool {
    argv.get(1).and_then(|a| a.to_str()) == Some(MIC_CAPTURE_SUBCOMMAND)
}

#[cfg(test)]
mod intercept_tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<std::ffi::OsString> {
        items.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn capture_subcommand_matches_only_argv1() {
        assert!(is_capture_subcommand(&argv(&[
            "echoagent",
            "__mic-capture"
        ])));
        assert!(is_capture_subcommand(&argv(&[
            "echoagent",
            "__mic-capture",
            "--rate",
            "16000"
        ])));
        assert!(!is_capture_subcommand(&argv(&["echoagent"])));
        assert!(!is_capture_subcommand(&argv(&["echoagent", "chat"])));
        assert!(!is_capture_subcommand(&argv(&[
            "echoagent",
            "chat",
            "__mic-capture"
        ])));
    }
}
