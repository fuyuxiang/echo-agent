//! Voice diagnostics: input-device lookup, silent-mic fix text, and an
//! end-to-end probe (mic → streaming STT → transcript).

#[cfg(feature = "audio")]
use std::sync::Arc;
#[cfg(feature = "audio")]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "audio")]
use std::time::Duration;

#[cfg(feature = "audio")]
use tokio::time::timeout;

use crate::auth::SharedVoiceAuth;
use crate::config::VoiceConfig;
use crate::error::VoiceError;
#[cfg(feature = "audio")]
use crate::stt::{StreamingSttEvent, StreamingSttSession};

/// Options for [`run_streaming_probe`].
#[derive(Debug, Clone)]
pub struct VoiceProbeOptions {
    pub config: VoiceConfig,
    pub auth: SharedVoiceAuth,
    /// How long to capture microphone audio before `audio.done`.
    pub capture_secs: u32,
}

/// Collected probe output.
#[derive(Debug)]
pub struct VoiceProbeReport {
    pub pcm_bytes: usize,
    pub stt_log: Vec<String>,
    pub transcript: Option<String>,
}

/// Capture mic audio and stream it to EchoAgent STT, reporting the transcript.
#[cfg(feature = "audio")]
pub async fn run_streaming_probe(opts: VoiceProbeOptions) -> Result<VoiceProbeReport, VoiceError> {
    let bearer = crate::auth::require_bearer(&opts.auth).await?;
    let mut stt = StreamingSttSession::connect(&opts.config, &bearer).await?;
    let stt_tx = stt
        .audio_sender()
        .ok_or_else(|| VoiceError::Stt("STT audio sender unavailable".into()))?;

    let byte_count = Arc::new(AtomicUsize::new(0));
    let byte_count_cb = Arc::clone(&byte_count);
    let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let forward = tokio::spawn(async move {
        let mut pcm_rx = pcm_rx;
        while let Some(chunk) = pcm_rx.recv().await {
            byte_count_cb.fetch_add(chunk.len(), Ordering::Relaxed);
            if stt_tx.send(chunk).await.is_err() {
                break;
            }
        }
    });

    let sample_rate = opts.config.sample_rate;
    let secs = opts.capture_secs.max(1);
    let capture = crate::audio::spawn_pcm_capture(sample_rate, pcm_tx)?;
    tracing::info!(secs, "speak now — probe is listening");
    tokio::time::sleep(Duration::from_secs(secs as u64)).await;
    capture.stop();
    let _ = forward.await;
    stt.finish_audio();

    let mut stt_log = Vec::new();
    let mut transcript = None;
    let deadline = Duration::from_secs(30);

    loop {
        let ev = match timeout(deadline, stt.recv()).await {
            Ok(Some(ev)) => ev,
            Ok(None) => {
                stt_log.push("STT channel closed".into());
                break;
            }
            Err(_) => {
                stt_log.push("STT recv timed out (30s)".into());
                break;
            }
        };

        match &ev {
            StreamingSttEvent::Ready => stt_log.push("STT: ready (transcript.created)".into()),
            StreamingSttEvent::Partial(p) => {
                stt_log.push(format!(
                    "STT: partial is_final={} speech_final={} text={:?}",
                    p.is_final, p.speech_final, p.text
                ));
                if !p.text.trim().is_empty() && (p.speech_final || p.is_final) {
                    transcript = Some(p.text.clone());
                }
            }
            StreamingSttEvent::Done { text } => {
                stt_log.push(format!("STT: done text={:?}", text));
                if !text.trim().is_empty() {
                    transcript = Some(text.clone());
                }
                break;
            }
            StreamingSttEvent::Error { message } => {
                stt_log.push(format!("STT: error {message}"));
                break;
            }
        }
    }

    let pcm_bytes = byte_count.load(Ordering::Relaxed);

    Ok(VoiceProbeReport {
        pcm_bytes,
        stt_log,
        transcript,
    })
}

/// Record mic only (no STT) — quick hardware check.
#[cfg(feature = "audio")]
pub fn run_mic_only_probe(sample_rate: u32, seconds: u32) -> Result<(usize, u32), VoiceError> {
    let (pcm, chunks) = crate::audio::capture_pcm_for_duration(sample_rate, seconds)?;
    Ok((pcm.len(), chunks))
}

#[cfg(not(feature = "audio"))]
pub async fn run_streaming_probe(_opts: VoiceProbeOptions) -> Result<VoiceProbeReport, VoiceError> {
    Err(VoiceError::Config(
        "voice probe requires the `audio` feature (cpal)".into(),
    ))
}

/// Input device capture would use (cpal default, or Linux recorder name).
/// Available without `audio` so `/terminal-setup` compiles in no-audio builds.
#[derive(Debug, Clone)]
pub struct InputDeviceInfo {
    pub name: String,
    pub detail: String,
}

/// Look up the input device without opening a stream (does not trigger the
/// macOS mic-permission prompt).
#[cfg(feature = "audio")]
pub fn input_device_info() -> Result<InputDeviceInfo, VoiceError> {
    crate::audio::input_device_info()
}

#[cfg(not(feature = "audio"))]
pub fn input_device_info() -> Result<InputDeviceInfo, VoiceError> {
    Err(VoiceError::Config(
        "voice audio capture disabled (build without `audio` feature)".into(),
    ))
}

/// Platform-specific fix text for a mic that isn't being picked up. On macOS
/// the grant is for the terminal app and only applies after that app restarts.
pub fn mic_fix_help() -> &'static str {
    if cfg!(target_os = "macos") {
        "Allow microphone access for your terminal in System Settings → Privacy & Security → \
         Microphone, then restart the terminal. If access is already on, check the input device \
         and level in System Settings → Sound → Input."
    } else if cfg!(target_os = "windows") {
        "allow microphone access in Settings → Privacy & security → \
         Microphone, and check the input device and level in Settings → \
         System → Sound."
    } else {
        "check the default input device and its volume in your sound settings \
         (e.g. `pavucontrol`, or `wpctl status` on PipeWire)."
    }
}

/// Human-readable multi-line report for terminal output.
pub fn format_probe_report(report: &VoiceProbeReport) -> String {
    let mut out = String::from("=== echo-agent-voice probe ===\n\n");

    out.push_str(&format!(
        "Mic capture (streamed)\n  pcm_bytes: {}\n",
        report.pcm_bytes
    ));
    if report.pcm_bytes == 0 {
        out.push_str("  WARNING: no PCM captured — check mic permission / default input device\n");
    } else {
        let secs_approx = report.pcm_bytes as f64 / (16000.0 * 2.0);
        out.push_str(&format!(
            "  approx duration: {secs_approx:.2}s @ 16kHz mono PCM16\n"
        ));
    }

    out.push_str("\nSTT events\n");
    if report.stt_log.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for line in &report.stt_log {
            out.push_str(&format!("  {line}\n"));
        }
    }

    out.push_str("\nTranscript\n");
    match &report.transcript {
        Some(t) if !t.trim().is_empty() => out.push_str(&format!("  {t}\n")),
        Some(_) => out.push_str("  (empty string)\n"),
        None => out.push_str("  (none — STT returned no text)\n"),
    }

    out
}
