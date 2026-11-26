//! Microphone capture on macOS via a short-lived self-exec helper process.
//!
//! Opening CoreAudio in-process permanently dirties the pager's memory
//! footprint: several MB for the HAL plus device capture buffers (tens of MB
//! with some input routes), none of it returned to the OS after the stream is
//! dropped. Capture therefore runs out of process, like the Linux recorder
//! backend: the pager spawns `current_exe __mic-capture --rate N`, the child
//! streams raw PCM16 mono LE to stdout behind a one-line `READY`/`ERR` header
//! (see [`super::capture::run_capture_child_cli`]), and all audio-stack
//! memory is freed when the child exits with the utterance. The helper is the
//! same executable, so the terminal's mic permission grant applies unchanged.
//!
//! In-process capture ([`super::capture`]) remains the fallback when the
//! helper cannot run at all — self-exec unavailable, or the spawned binary
//! doesn't speak the helper protocol (e.g. it was replaced by an update mid
//! run) — and can be forced with `ECHO_AGENT_VOICE_CAPTURE=inprocess`.

use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio::sync::mpsc as async_mpsc;

use super::pipe::{self, ChildCaptureHandle};
use super::protocol;
use crate::error::VoiceError;

/// Env escape hatch: `ECHO_AGENT_VOICE_CAPTURE=inprocess` forces the legacy
/// in-process cpal backend (accepting its permanent footprint cost).
const CAPTURE_BACKEND_ENV: &str = "ECHO_AGENT_VOICE_CAPTURE";

/// How long to wait for the helper's status header. Device open takes
/// hundreds of ms; exec of the (usually page-cached) binary adds tens more.
/// Matches the in-process backend's 5 s open handshake.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Stop handle for a capture session: the helper child, or the in-process
/// fallback stream.
pub enum CaptureHandle {
    Child(ChildCaptureHandle),
    InProcess(super::capture::CaptureHandle),
}

impl CaptureHandle {
    /// Stop capture and wait until the device is released.
    pub fn stop(self) {
        match self {
            CaptureHandle::Child(h) => h.stop(),
            CaptureHandle::InProcess(h) => h.stop(),
        }
    }
}

/// Whether the env escape hatch forces the in-process backend.
fn force_inprocess() -> bool {
    std::env::var(CAPTURE_BACKEND_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("inprocess"))
}

/// Why the helper handshake produced no `READY`/`INFO` payload.
#[derive(Debug)]
enum HandshakeFailure {
    /// The helper ran and reported `ERR` (a real device/permission error), or
    /// timed out opening the device. Surfaced as-is; an in-process retry
    /// would fail identically.
    Reported(VoiceError),
    /// The helper could not run or doesn't speak the protocol (spawn failure,
    /// EOF/garbage/oversized header — e.g. the binary was replaced by an
    /// update mid-run). The caller falls back to in-process capture.
    Broken(VoiceError),
}

/// Spawn the helper (detached from the TTY, stdin null, stdout/stderr piped)
/// and hand back its stdout. Kills the child on the defensive missing-stdout
/// path so it can never outlive the error.
fn spawn_helper(args: &[&str]) -> Result<(Child, ChildStdout), VoiceError> {
    let exe = std::env::current_exe()
        .map_err(|e| VoiceError::Config(format!("current_exe for mic helper: {e}")))?;
    let mut cmd = Command::new(exe);
    cmd.arg(crate::MIC_CAPTURE_SUBCOMMAND)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The helper must not share the pager's controlling TTY.
    echo_agent_tty_utils::detach_std_command(&mut cmd);

    #[allow(clippy::disallowed_methods)] // helper owned by the capture handle, killed on stop
    let mut child = cmd
        .spawn()
        .map_err(|e| VoiceError::Config(format!("spawn mic helper: {e}")))?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(VoiceError::Config("mic helper produced no stdout".into()));
    };
    pipe::drain_stderr(&mut child, "mic-helper");
    Ok((child, stdout))
}

/// Read the helper's one-line status header. Byte-at-a-time so no PCM after
/// the newline is consumed from the stream.
fn read_header(stdout: &mut impl Read) -> Result<String, HandshakeFailure> {
    let mut line = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    // Cap far above any real header so a corrupt child can't feed us forever.
    while line.len() < 4096 {
        match stdout.read(&mut byte) {
            Ok(0) => {
                return Err(HandshakeFailure::Broken(VoiceError::Config(
                    "mic helper exited before ready".into(),
                )));
            }
            Ok(_) if byte[0] == b'\n' => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches('\r');
                return match text.split_once(' ') {
                    Some((tag, payload)) if tag == protocol::READY || tag == protocol::INFO => {
                        Ok(payload.to_string())
                    }
                    Some((tag, message)) if tag == protocol::ERR => Err(
                        HandshakeFailure::Reported(VoiceError::Config(message.to_string())),
                    ),
                    _ => Err(HandshakeFailure::Broken(VoiceError::Config(format!(
                        "unexpected mic helper header: {text:?}"
                    )))),
                };
            }
            Ok(_) => line.push(byte[0]),
            Err(e) => {
                return Err(HandshakeFailure::Broken(VoiceError::Config(format!(
                    "read mic helper header: {e}"
                ))));
            }
        }
    }
    Err(HandshakeFailure::Broken(VoiceError::Config(
        "oversized mic helper header".into(),
    )))
}

/// Kill + reap a handshake-failed child, then join its reader (the kill
/// closes stdout, so a blocked header read returns EOF and the join
/// completes).
fn teardown(mut child: Child, reader: JoinHandle<()>) {
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
}

/// Run the handshake with a deadline: a reader thread does the blocking read
/// and sends the outcome (plus the stdout, for the PCM stream that follows)
/// over a channel. On failure the child is killed, reaped, and joined.
/// `timeout_what` names the operation in the timeout error (capture vs
/// device-info).
fn handshake(
    child: Child,
    mut stdout: ChildStdout,
    timeout_what: &str,
) -> Result<(Child, String, ChildStdout), HandshakeFailure> {
    type Outcome = (Result<String, HandshakeFailure>, ChildStdout);
    let (tx, rx) = std::sync::mpsc::sync_channel::<Outcome>(1);
    let reader = thread::spawn(move || {
        let outcome = read_header(&mut stdout);
        let _ = tx.send((outcome, stdout));
    });

    // A result that lands just as the timeout fires must not be discarded as
    // a timeout, so the deadline arm re-checks the channel once before
    // tearing down.
    let outcome = rx
        .recv_timeout(READY_TIMEOUT)
        .or_else(|_| rx.try_recv())
        .map_err(|_| {
            HandshakeFailure::Reported(VoiceError::Config(format!(
                "{timeout_what} did not start within {}s",
                READY_TIMEOUT.as_secs()
            )))
        });
    match outcome {
        Ok((Ok(payload), stdout)) => {
            let _ = reader.join();
            Ok((child, payload, stdout))
        }
        Ok((Err(failure), _stdout)) => {
            teardown(child, reader);
            Err(failure)
        }
        Err(timeout) => {
            teardown(child, reader);
            Err(timeout)
        }
    }
}

/// Spawn helper capture; PCM16 LE chunks are forwarded to `pcm_tx`.
///
/// Falls back to in-process cpal capture when the helper cannot run at all
/// (spawn failure or broken protocol). Device/permission errors reported by a
/// working helper — and handshake timeouts, which an in-process retry of the
/// same stuck device would only double — surface as-is.
pub fn spawn_pcm_capture(
    sample_rate: u32,
    pcm_tx: async_mpsc::Sender<Vec<u8>>,
) -> Result<CaptureHandle, VoiceError> {
    if force_inprocess() {
        tracing::info!("voice capture forced in-process ({CAPTURE_BACKEND_ENV}=inprocess)");
        return super::capture::spawn_pcm_capture(sample_rate, pcm_tx)
            .map(CaptureHandle::InProcess);
    }

    let rate = sample_rate.to_string();
    let handshaken = spawn_helper(&["--rate", &rate])
        .map_err(HandshakeFailure::Broken)
        .and_then(|(child, stdout)| handshake(child, stdout, "voice capture"));

    let (child, device, stdout) = match handshaken {
        Ok(up) => up,
        Err(HandshakeFailure::Broken(e)) => {
            tracing::warn!(error = %e, "mic helper unavailable; falling back to in-process capture");
            return super::capture::spawn_pcm_capture(sample_rate, pcm_tx)
                .map(CaptureHandle::InProcess);
        }
        Err(HandshakeFailure::Reported(e)) => return Err(e),
    };

    tracing::info!(
        device = %device,
        sample_rate,
        "voice capture stream (mic helper subprocess)"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stop_reader = Arc::clone(&stop);
    let reader =
        thread::spawn(move || pipe::forward_pcm(stdout, pcm_tx, stop_reader, "mic-helper"));
    Ok(CaptureHandle::Child(ChildCaptureHandle::new(
        child, stop, reader,
    )))
}

/// Default input device via the helper (`--device-info`), so `/doctor` in the
/// long-lived TUI doesn't pay the permanent in-process CoreAudio enumeration
/// cost. Falls back to in-process enumeration when the helper cannot run.
pub fn input_device_info() -> Result<crate::probe::InputDeviceInfo, VoiceError> {
    if force_inprocess() {
        return super::capture::input_device_info();
    }
    let handshaken = spawn_helper(&["--device-info"])
        .map_err(HandshakeFailure::Broken)
        .and_then(|(child, stdout)| handshake(child, stdout, "mic device lookup"));

    let payload = match handshaken {
        Ok((mut child, payload, _stdout)) => {
            // Info mode: the child prints its one line and exits on its own.
            // Kill defensively before reaping (a no-op when already exited) so
            // a confused child that streams PCM can never wedge the `wait`.
            let _ = child.kill();
            let _ = child.wait();
            payload
        }
        Err(HandshakeFailure::Broken(e)) => {
            tracing::debug!(error = %e, "mic helper unavailable; enumerating in-process");
            return super::capture::input_device_info();
        }
        Err(HandshakeFailure::Reported(e)) => return Err(e),
    };

    let (name, detail) = payload
        .split_once(protocol::INFO_FIELD_SEPARATOR)
        .unwrap_or((payload.as_str(), ""));
    Ok(crate::probe::InputDeviceInfo {
        name: name.to_string(),
        detail: detail.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(bytes: &[u8]) -> Result<String, HandshakeFailure> {
        read_header(&mut std::io::Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn header_parses_ready_info_and_err() {
        let mut ok = std::io::Cursor::new(b"READY Built-in Microphone\nPCM".to_vec());
        assert_eq!(read_header(&mut ok).unwrap(), "Built-in Microphone");
        // The PCM byte after the newline must remain unread.
        let mut rest = Vec::new();
        ok.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"PCM");

        assert_eq!(
            header(b"INFO Mic\t44100 Hz, 1 ch\n").unwrap(),
            "Mic\t44100 Hz, 1 ch"
        );

        match header(b"ERR no default input audio device\n") {
            Err(HandshakeFailure::Reported(VoiceError::Config(msg))) => {
                assert_eq!(msg, "no default input audio device");
            }
            other => panic!("expected Reported, got {other:?}"),
        }
    }

    #[test]
    fn header_treats_eof_garbage_and_oversize_as_broken() {
        for bytes in [
            b"".as_slice(),               // EOF before any header
            b"bogus header\n".as_slice(), // unknown tag
            &[b'x'; 5000],                // no newline within the cap
        ] {
            assert!(
                matches!(header(bytes), Err(HandshakeFailure::Broken(_))),
                "input {:?}... must be Broken",
                &bytes[..bytes.len().min(12)]
            );
        }
    }

    /// Drive `handshake` against real scripted children, covering the
    /// concurrent recv/teardown paths that the pure header tests cannot:
    /// success (with the stdout handed back intact), a reported error, a
    /// protocol-broken child, and a child that never answers (timeout).
    #[test]
    fn handshake_resolves_scripted_children() {
        let spawn_sh = |script: &str| -> (Child, ChildStdout) {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            echo_agent_tty_utils::detach_std_command(&mut cmd);
            #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
            let mut child = cmd.spawn().expect("spawn sh");
            let stdout = child.stdout.take().expect("stdout");
            (child, stdout)
        };

        // READY → payload plus the byte stream after the header, unconsumed.
        let (child, stdout) = spawn_sh("printf 'READY fake-mic\\nPCM'; sleep 5");
        let (mut child, payload, mut stdout) =
            handshake(child, stdout, "test").expect("ready handshake");
        assert_eq!(payload, "fake-mic");
        let mut pcm = [0u8; 3];
        stdout.read_exact(&mut pcm).expect("post-header bytes");
        assert_eq!(&pcm, b"PCM");
        let _ = child.kill();
        let _ = child.wait();

        // ERR → Reported, child reaped by handshake.
        let (child, stdout) = spawn_sh("printf 'ERR no such device\\n'");
        match handshake(child, stdout, "test") {
            Err(HandshakeFailure::Reported(VoiceError::Config(msg))) => {
                assert_eq!(msg, "no such device");
            }
            Ok(_) => panic!("expected Reported, got READY"),
            Err(other) => panic!("expected Reported, got {other:?}"),
        }

        // Garbage → Broken (the in-process fallback trigger).
        let (child, stdout) = spawn_sh("printf 'not-a-header\\n'");
        assert!(matches!(
            handshake(child, stdout, "test"),
            Err(HandshakeFailure::Broken(_))
        ));
    }

    /// A child that produces no header within the deadline is killed and the
    /// timeout surfaces as `Reported`, naming the caller's operation. Costs a
    /// full `READY_TIMEOUT` (5 s), so it is ignored by default.
    #[test]
    #[ignore = "takes READY_TIMEOUT (5s); run with --ignored"]
    fn handshake_times_out_on_silent_child() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        echo_agent_tty_utils::detach_std_command(&mut cmd);
        #[allow(clippy::disallowed_methods)] // test fixture; the test kills it
        let mut child = cmd.spawn().expect("spawn sh");
        let stdout = child.stdout.take().expect("stdout");
        match handshake(child, stdout, "test capture") {
            Err(HandshakeFailure::Reported(VoiceError::Config(msg))) => {
                assert!(msg.contains("test capture"), "{msg}");
                assert!(msg.contains("did not start"), "{msg}");
            }
            Ok(_) => panic!("expected timeout, got READY"),
            Err(other) => panic!("expected timeout Reported, got {other:?}"),
        }
    }
}
