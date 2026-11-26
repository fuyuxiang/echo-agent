//! Microphone capture on Linux via a subprocess recorder.
//!
//! The release CLI ships as a fully-static `*-unknown-linux-musl` binary, so it
//! cannot link `cpal` -> `alsa-sys` (a `NEEDED libasound.so.2`) without losing
//! the static guarantee enforced by the release build. Statically linking ALSA
//! is no help either: it reaches the user's real device (PulseAudio/PipeWire)
//! through plugins it loads via `dlopen`, which a static musl binary can't do.
//!
//! Instead, capture mic audio by spawning the system recorder (`pw-record`,
//! `parec`, or `arecord`) and reading raw PCM16 mono from its stdout — no native
//! audio library is linked into the binary at all. The recorders are asked for
//! signed 16-bit little-endian mono at the STT sample rate, which is exactly the
//! format the pipeline forwards, so there is no downmix/resample step.
//!
//! This module exposes the same interface as the `cpal` backend
//! (`spawn_pcm_capture`, `capture_pcm_for_duration`, `CaptureHandle`) so the
//! pipeline and probe are backend-agnostic.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::mpsc as async_mpsc;

use super::pipe::{self, READ_CHUNK};
use crate::error::VoiceError;

/// How long to wait after spawning before deciding the recorder started cleanly.
/// A missing device or a stopped audio server makes the recorder exit within a
/// few ms; this surfaces that as an error instead of a session that "listens"
/// but never produces audio (mirrors the `cpal` backend's open handshake).
const START_GRACE: Duration = Duration::from_millis(300);

/// A system audio recorder that can stream raw PCM16 mono to stdout.
#[derive(Clone, Copy)]
enum Recorder {
    /// PipeWire's `pw-record`.
    PwRecord,
    /// PulseAudio's `parec`.
    Parec,
    /// ALSA's `arecord` (alsa-utils).
    Arecord,
}

impl Recorder {
    fn program(self) -> &'static str {
        match self {
            Recorder::PwRecord => "pw-record",
            Recorder::Parec => "parec",
            Recorder::Arecord => "arecord",
        }
    }

    /// Args that emit signed 16-bit little-endian mono PCM at `rate` Hz to
    /// stdout. (`pw-record`/`pw-cat` and `arecord` take an explicit `-` stdout
    /// target; `parec` writes raw to stdout by default.)
    fn args(self, rate: u32) -> Vec<String> {
        let rate = rate.to_string();
        match self {
            Recorder::PwRecord => vec![
                // `--raw` is load-bearing: without it `pw-record` treats
                // `--format`/`--rate`/`--channels` as a libsndfile container
                // subformat and wraps stdout in a container — WAV on
                // PipeWire < 1.6 (unwritable to a pipe: "this file format
                // does not support pipe writing", exit 1 — e.g. Ubuntu 24.04
                // / Debian 12 ship 1.0/1.2), AU with a header on ≥ 1.6. Raw
                // mode fwrites pure PCM16 frames, which is what the reader
                // expects from every backend.
                "--raw".into(),
                "--rate".into(),
                rate,
                "--channels".into(),
                "1".into(),
                "--format".into(),
                "s16".into(),
                "-".into(),
            ],
            Recorder::Parec => vec![
                "--raw".into(),
                "--format=s16le".into(),
                format!("--rate={rate}"),
                "--channels=1".into(),
            ],
            Recorder::Arecord => vec![
                "-q".into(),
                "-t".into(),
                "raw".into(),
                "-f".into(),
                "S16_LE".into(),
                "-c".into(),
                "1".into(),
                "-r".into(),
                rate,
                "-".into(),
            ],
        }
    }
}

/// First recorder found on `PATH`, preferring PipeWire > PulseAudio > ALSA so we
/// go through the user's configured audio server (and its default input device)
/// rather than grabbing a raw ALSA `hw:` device.
fn detect_recorder() -> Option<Recorder> {
    detect_recorder_with(binary_on_path)
}

/// [`detect_recorder`] with the `PATH` probe injected, so the preference order
/// is unit-testable without process-global `PATH` mutation.
fn detect_recorder_with(available: impl Fn(&str) -> bool) -> Option<Recorder> {
    [Recorder::PwRecord, Recorder::Parec, Recorder::Arecord]
        .into_iter()
        .find(|r| available(r.program()))
}

/// Whether `name` resolves to an executable regular file on any `PATH` entry
/// (so a stray non-executable file can't shadow a working recorder).
fn binary_on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        dir.join(name)
            .metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// The detected recorder, or a `VoiceError` naming the packages to install.
fn require_recorder() -> Result<Recorder, VoiceError> {
    detect_recorder().ok_or_else(|| {
        VoiceError::Config(
            "no microphone recorder found on PATH: install pipewire (pw-record), \
             pulseaudio-utils (parec), or alsa-utils (arecord)"
                .into(),
        )
    })
}

/// Spawn the chosen recorder with stdout/stderr piped, and confirm it didn't
/// exit immediately (no device, audio server down). On success the child is
/// running with `stdout` available for reading.
fn spawn_recorder(sample_rate: u32) -> Result<(Recorder, Child), VoiceError> {
    let recorder = require_recorder()?;

    let mut cmd = Command::new(recorder.program());
    cmd.args(recorder.args(sample_rate))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // setsid detach via the sanctioned helper (workspace subprocess rule): the
    // recorder writes to a pipe and must not share the pager's controlling TTY.
    echo_agent_tty_utils::detach_std_command(&mut cmd);
    #[allow(clippy::disallowed_methods)] // recorder owned by the capture handle, killed on stop
    let mut child = cmd
        .spawn()
        .map_err(|e| VoiceError::Config(format!("failed to start {}: {e}", recorder.program())))?;

    thread::sleep(START_GRACE);
    match child.try_wait() {
        Ok(Some(status)) => {
            let mut stderr = String::new();
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_string(&mut stderr);
            }
            let stderr = stderr.trim();
            Err(VoiceError::Config(format!(
                "{} exited immediately ({status}){}",
                recorder.program(),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                },
            )))
        }
        Ok(None) => Ok((recorder, child)),
        Err(e) => Err(VoiceError::Config(format!(
            "failed to poll {}: {e}",
            recorder.program()
        ))),
    }
}

/// Stop handle for the recorder subprocess (owns the child + reader thread).
pub use super::pipe::ChildCaptureHandle as CaptureHandle;

/// Spawn subprocess capture; PCM16 LE chunks are forwarded to `pcm_tx`.
pub fn spawn_pcm_capture(
    sample_rate: u32,
    pcm_tx: async_mpsc::Sender<Vec<u8>>,
) -> Result<CaptureHandle, VoiceError> {
    let (recorder, mut child) = spawn_recorder(sample_rate)?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(VoiceError::Config(format!(
            "{} produced no stdout",
            recorder.program()
        )));
    };

    pipe::drain_stderr(&mut child, recorder.program());

    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = Arc::clone(&stop);
    let device = recorder.program();
    let reader = thread::spawn(move || pipe::forward_pcm(stdout, pcm_tx, stop_reader, device));

    tracing::info!(
        recorder = recorder.program(),
        sample_rate,
        "voice capture stream (subprocess)"
    );

    Ok(CaptureHandle::new(child, stop, reader))
}

/// Recorder that would be spawned, without recording ([`crate::probe::input_device_info`]).
pub fn input_device_info() -> Result<crate::probe::InputDeviceInfo, VoiceError> {
    let recorder = require_recorder()?;
    Ok(crate::probe::InputDeviceInfo {
        name: recorder.program().to_string(),
        detail: "system recorder; uses the audio server's default input".to_string(),
    })
}

/// Record mono PCM16 LE for a fixed duration (probe / diagnostics).
pub fn capture_pcm_for_duration(
    sample_rate: u32,
    seconds: u32,
) -> Result<(Vec<u8>, u32), VoiceError> {
    let (recorder, mut child) = spawn_recorder(sample_rate)?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(VoiceError::Config(format!(
            "{} produced no stdout",
            recorder.program()
        )));
    };
    pipe::drain_stderr(&mut child, recorder.program());

    let duration = Duration::from_secs(seconds.max(1) as u64);
    let deadline = Instant::now() + duration;

    // Watchdog: kill the recorder at the deadline so a `read` that is blocked
    // waiting for PCM (recorder alive but idle / stalled pipe) gets EOF instead
    // of running past the requested duration. Killing at the deadline also ends
    // a healthy capture, so the read loop below needs no between-read deadline
    // check beyond its backstop.
    // Deliberately not joined: if the recorder dies early we return without
    // waiting out the full duration, and the watchdog's late `kill` on an
    // already-reaped `Child` is a harmless `InvalidInput` (std tracks the reap,
    // so no PID-reuse hazard).
    let child = Arc::new(Mutex::new(child));
    let watchdog_child = Arc::clone(&child);
    thread::spawn(move || {
        thread::sleep(duration);
        let mut child = watchdog_child.lock().expect("watchdog lock poisoned");
        let _ = child.kill();
    });

    let mut pcm = Vec::new();
    let mut chunks = 0u32;
    let mut buf = vec![0u8; READ_CHUNK];
    // Small slack past the deadline: the kill's EOF (`Ok(0)`) is the intended
    // exit; the time check is a backstop against a pathological pipe.
    while Instant::now() < deadline + Duration::from_secs(1) {
        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                chunks += 1;
                pcm.extend_from_slice(&buf[..n]);
            }
            Err(_) => break,
        }
    }

    {
        let mut child = child.lock().expect("child lock poisoned");
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok((pcm, chunks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arecord_args_are_raw_s16_mono() {
        let args = Recorder::Arecord.args(16_000);
        assert!(args.contains(&"S16_LE".to_string()));
        assert!(args.contains(&"raw".to_string()));
        // mono
        let c = args.iter().position(|a| a == "-c").unwrap();
        assert_eq!(args[c + 1], "1");
        // rate
        let r = args.iter().position(|a| a == "-r").unwrap();
        assert_eq!(args[r + 1], "16000");
        // stdout target
        assert_eq!(args.last().unwrap(), "-");
    }

    #[test]
    fn parec_and_pw_args_carry_rate_format_and_mono() {
        let parec = Recorder::Parec.args(24_000);
        assert!(parec.contains(&"--raw".to_string()));
        assert!(parec.contains(&"--format=s16le".to_string()));
        assert!(parec.contains(&"--rate=24000".to_string()));
        assert!(parec.contains(&"--channels=1".to_string()));

        let pw = Recorder::PwRecord.args(48_000);
        // Raw mode is required: without it pw-record wraps stdout in a
        // libsndfile container (WAV on PipeWire < 1.6, which cannot be
        // written to a pipe at all; AU with a header on >= 1.6).
        assert!(pw.contains(&"--raw".to_string()));
        let r = pw.iter().position(|a| a == "--rate").unwrap();
        assert_eq!(pw[r + 1], "48000");
        let f = pw.iter().position(|a| a == "--format").unwrap();
        assert_eq!(pw[f + 1], "s16");
        let c = pw.iter().position(|a| a == "--channels").unwrap();
        assert_eq!(pw[c + 1], "1");
        assert_eq!(pw.last().unwrap(), "-"); // stdout target
    }

    #[test]
    fn recorder_preference_is_pipewire_then_pulse_then_alsa() {
        // All present: PipeWire wins (routes through the user's audio server).
        let all = detect_recorder_with(|_| true);
        assert!(matches!(all, Some(Recorder::PwRecord)));

        // No PipeWire: PulseAudio next.
        let no_pw = detect_recorder_with(|p| p != "pw-record");
        assert!(matches!(no_pw, Some(Recorder::Parec)));

        // alsa-utils only: arecord is the last resort.
        let alsa_only = detect_recorder_with(|p| p == "arecord");
        assert!(matches!(alsa_only, Some(Recorder::Arecord)));

        assert!(detect_recorder_with(|_| false).is_none());
    }
}
