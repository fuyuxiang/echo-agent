//! Shared PCM-over-pipe plumbing for the subprocess capture backends
//! (Linux system recorder, macOS `__mic-capture` helper): the capture child's
//! stop handle, a reader-thread loop that forwards the child's stdout to the
//! async STT sender, and a stderr drain.

use std::io::Read;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use tokio::sync::mpsc as async_mpsc;

/// Stop handle for a capture child process (recorder or self-exec helper) and
/// its PCM reader thread.
pub struct ChildCaptureHandle {
    /// `Some` until `stop()` or `Drop` consumes it (kill + reap).
    child: Option<Child>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl ChildCaptureHandle {
    pub(super) fn new(child: Child, stop: Arc<AtomicBool>, reader: JoinHandle<()>) -> Self {
        Self {
            child: Some(child),
            stop,
            reader: Some(reader),
        }
    }

    /// Stop capture: kill the child, reap it, and join the reader thread so
    /// the input device is released before returning.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for ChildCaptureHandle {
    fn drop(&mut self) {
        // Always kill the child so the mic is released even when `stop()` was
        // never called (e.g. the STT session ended on its own). Killing closes
        // the child's stdout, so the reader thread's blocking `read` returns 0
        // and it exits. `Drop` must never block (it may run on an async
        // executor), so the reap happens on a detached thread — without it,
        // drop-path teardowns would leave zombies until the pager exits.
        self.stop.store(true, Ordering::Release);
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // `Builder::spawn` so spawn failure under thread exhaustion
            // degrades to kill-without-reap instead of a panicking `Drop`.
            let _ = thread::Builder::new()
                .name("voice-capture-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

/// PCM read size from the child's stdout (bytes) — ~64 ms at 16 kHz mono
/// PCM16. Small enough to stream responsively, large enough to avoid syscall
/// churn on the reader thread.
pub(super) const READ_CHUNK: usize = 2048;

/// Forward raw PCM from the child's stdout to the async STT sender until the
/// child stops (EOF on kill), the consumer goes away, or `stop` is set.
/// Generic over the reader for tests; production passes the child's stdout.
pub(super) fn forward_pcm(
    mut stdout: impl Read,
    pcm_tx: async_mpsc::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    device: &'static str,
) {
    let mut buf = vec![0u8; READ_CHUNK];
    let mut dropped = 0u64;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match stdout.read(&mut buf) {
            // EOF: the child closed stdout (killed by teardown or exited).
            Ok(0) => break,
            Ok(n) => {
                // Never park this thread on the channel: `stop()` joins it, so
                // a send that waits on a stalled STT consumer would turn
                // teardown into a hang. Shed load instead. (`read` itself is
                // unblocked by the kill-on-stop path: killing the child closes
                // stdout, so a waiting `read` returns 0.)
                match pcm_tx.try_send(buf[..n].to_vec()) {
                    Ok(()) => {}
                    Err(async_mpsc::error::TrySendError::Full(_)) => dropped += 1,
                    // Consumer is gone: the session ended; stop capturing.
                    Err(async_mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
            Err(e) => {
                tracing::warn!(device, error = %e, "voice capture read error");
                break;
            }
        }
    }
    if dropped > 0 {
        tracing::warn!(
            device,
            dropped,
            "voice capture dropped PCM chunks (slow consumer)"
        );
    }
}

/// Drain the child's stderr to EOF on a detached thread so a chatty child
/// can't fill the pipe buffer and block its own writes (the hot path never
/// reads stderr). Non-empty output is logged at debug. The thread ends on its
/// own when the child exits, so it is not joined.
pub(super) fn drain_stderr(child: &mut Child, device: &'static str) {
    let Some(mut stderr) = child.stderr.take() else {
        return;
    };
    thread::spawn(move || {
        let mut buf = String::new();
        if stderr.read_to_string(&mut buf).is_ok() {
            let msg = buf.trim();
            if !msg.is_empty() {
                tracing::debug!(device, stderr = msg, "voice capture child stderr");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_pcm_sheds_when_consumer_is_behind() {
        // Two reads into a capacity-1 channel: first forwarded, second shed.
        let pcm = vec![7u8; 2 * READ_CHUNK];
        let (tx, mut rx) = async_mpsc::channel::<Vec<u8>>(1);
        forward_pcm(
            std::io::Cursor::new(pcm),
            tx,
            Arc::new(AtomicBool::new(false)),
            "test",
        );
        assert_eq!(rx.try_recv().expect("first chunk").len(), READ_CHUNK);
        assert!(rx.try_recv().is_err(), "second chunk shed (channel full)");
    }

    #[test]
    fn forward_pcm_stops_when_consumer_closes() {
        let (tx, rx) = async_mpsc::channel::<Vec<u8>>(1);
        drop(rx);
        // Endless reader: must exit via the Closed arm, not spin forever.
        forward_pcm(
            std::io::repeat(0),
            tx,
            Arc::new(AtomicBool::new(false)),
            "test",
        );
    }
}
