//! Voice pipeline: mic → streaming STT → pager events.
//!
//! The pager drives capture with press/release commands. They back both a
//! toggle (`/voice`, `Ctrl+Shift+M`) and true push-to-talk (F12 hold) — hence
//! the `Ptt*` names — so a press may be followed by a release after a long hold
//! or, for a toggle, a later stop.

#[cfg(feature = "audio")]
use std::collections::VecDeque;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::auth::SharedVoiceAuth;
use crate::config::VoiceConfig;
use crate::error::VoiceError;
use crate::event::VoiceEvent;
#[cfg(feature = "audio")]
use crate::stt::{StreamingSttEvent, StreamingSttSession};

/// Commands from the pager event loop (toggle start/stop, or F12 push-to-talk).
#[derive(Debug)]
pub enum VoiceCommand {
    /// Begin streaming audio to STT (mic open until [`VoiceCommand::PttRelease`]).
    PttPress,
    /// End the current capture session (`audio.done`, release mic).
    PttRelease,
    /// Tear down the pipeline task.
    Shutdown,
}

struct ActivePtt {
    finish_tx: mpsc::Sender<()>,
    reader: JoinHandle<()>,
}

/// Run until [`VoiceCommand::Shutdown`].
pub async fn run_voice_pipeline(
    config: VoiceConfig,
    auth: SharedVoiceAuth,
    mut cmd_rx: mpsc::Receiver<VoiceCommand>,
    event_tx: mpsc::Sender<VoiceEvent>,
) {
    let mut active: Option<ActivePtt> = None;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            VoiceCommand::Shutdown => break,
            VoiceCommand::PttPress => {
                // Supersede any prior session — including one still draining its
                // trailing final after a `PttRelease` — rather than ignoring the
                // press. A rapid stop→start would otherwise be dropped here while
                // the pager already flipped to "listening", leaving a dead mic
                // behind a recording UI and letting the old session's final land
                // on the new target. Aborting drops the old reader's capture +
                // STT session, releasing the mic and socket at once. (The pager
                // always sends a `PttRelease` between presses, so an `active`
                // session here is one that's stopping, never a live duplicate.)
                // We don't join the old reader, so its stream may still be
                // releasing as the new one opens — a brief overlap cpal handles.
                if let Some(prev) = active.take() {
                    prev.reader.abort();
                }

                // Connect + device-open take hundreds of ms. Race them against
                // the next command so a release/stop (or shutdown) arriving
                // mid-connect cancels the start — otherwise a quick tap-and-
                // release would open a hot mic and append a spurious final after
                // the user already let go. `biased` polls the start first so a
                // just-completed session is always kept (dropping it would leak
                // its reader). Dropping an unfinished start cancels the connect;
                // the concurrent mic-open still completes but its handle is then
                // dropped, releasing the device right away.
                tokio::select! {
                    biased;
                    session = open_session(&config, &auth, &event_tx) => {
                        active = session;
                    }
                    next = cmd_rx.recv() => match next {
                        // Released before capture was ready → cancel the start.
                        Some(VoiceCommand::PttRelease) => {}
                        Some(VoiceCommand::Shutdown) | None => break,
                        // Unreachable per the release-between-presses contract;
                        // start fresh defensively.
                        Some(VoiceCommand::PttPress) => {
                            active = open_session(&config, &auth, &event_tx).await;
                        }
                    },
                }
            }
            VoiceCommand::PttRelease => {
                let Some(session) = active.as_ref() else {
                    continue;
                };
                // The reader task owns the capture handle; signalling it lets the
                // reader stop the mic and send `audio.done` in a single place,
                // matching the no-speech-watchdog teardown below.
                let _ = session.finish_tx.send(()).await;
            }
        }
    }

    if let Some(session) = active {
        session.reader.abort();
    }
}

/// Open a capture session, emitting a `VoiceEvent::Error` (and returning `None`)
/// on failure. Extracted so the `PttPress` start can be raced against an
/// incoming release in `select!` and reused for the defensive restart path.
async fn open_session(
    config: &VoiceConfig,
    auth: &SharedVoiceAuth,
    event_tx: &mpsc::Sender<VoiceEvent>,
) -> Option<ActivePtt> {
    match start_capture_session(config, auth, event_tx).await {
        Ok(session) => Some(session),
        Err(e) => {
            let _ = event_tx
                .send(VoiceEvent::Error {
                    message: e.to_string(),
                    hint: None,
                })
                .await;
            None
        }
    }
}

#[cfg(not(feature = "audio"))]
async fn start_capture_session(
    _config: &VoiceConfig,
    _auth: &SharedVoiceAuth,
    _event_tx: &mpsc::Sender<VoiceEvent>,
) -> Result<ActivePtt, VoiceError> {
    Err(VoiceError::Config(
        "voice audio capture disabled (build without `audio` feature)".into(),
    ))
}

/// Hard cap on the pre-connect PCM backlog (memory safety). Sized far above any
/// real connect — the STT connect timeout aborts long before this is reached, so
/// in practice it never drops; it only bounds a pathological hang.
#[cfg(feature = "audio")]
const BACKLOG_MAX_CHUNKS: usize = 1024;

/// Bridge mic PCM into the STT socket across the connect handshake.
///
/// Until `audio_tx_rx` yields the live STT sender, captured chunks accumulate in
/// a bounded backlog (so the mic never backpressures while the socket connects);
/// once it arrives the backlog is flushed in order and capture streams live.
/// Holding the sender also defers the writer's `audio.done` until the backlog is
/// drained on teardown. Returns when the mic stops (`mic_rx` closed), the socket
/// goes away (`audio_tx` closed), or connect fails (`audio_tx_rx` dropped).
#[cfg(feature = "audio")]
async fn forward_pcm(
    mut mic_rx: mpsc::Receiver<Vec<u8>>,
    mut audio_tx_rx: tokio::sync::oneshot::Receiver<mpsc::Sender<Vec<u8>>>,
) {
    let mut backlog: VecDeque<Vec<u8>> = VecDeque::new();
    let audio_tx = loop {
        tokio::select! {
            chunk = mic_rx.recv() => match chunk {
                // A normal connect stays well under the cap, so the lead-in is
                // kept intact; only a pathologically slow connect (which the
                // connect timeout aborts anyway) drops its oldest chunks rather
                // than letting the buffer grow unbounded.
                Some(c) => {
                    if backlog.len() == BACKLOG_MAX_CHUNKS {
                        backlog.pop_front();
                    }
                    backlog.push_back(c);
                }
                None => return, // mic stopped before the socket was ready
            },
            tx = &mut audio_tx_rx => match tx {
                Ok(tx) => break tx,
                Err(_) => return, // connect failed → sender dropped
            },
        }
    };
    for chunk in backlog {
        if audio_tx.send(chunk).await.is_err() {
            return;
        }
    }
    while let Some(chunk) = mic_rx.recv().await {
        if audio_tx.send(chunk).await.is_err() {
            break;
        }
    }
}

/// How long a session may run without any transcript before it is torn down
/// (instead of streaming a dead mic until the user gives up). Disarmed by the
/// first transcript, so long dictation with pauses is unaffected.
#[cfg(feature = "audio")]
const NO_SPEECH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Message and permission guidance for a session torn down by
/// [`NO_SPEECH_TIMEOUT`]. A denied grant is indistinguishable from not speaking
/// because macOS may return silence instead of an error.
#[cfg(feature = "audio")]
fn no_speech_error() -> (String, Option<String>) {
    (
        "No speech was detected. Voice stopped.".to_owned(),
        Some(crate::probe::mic_fix_help().to_owned()),
    )
}

#[cfg(feature = "audio")]
async fn start_capture_session(
    config: &VoiceConfig,
    auth: &SharedVoiceAuth,
    event_tx: &mpsc::Sender<VoiceEvent>,
) -> Result<ActivePtt, VoiceError> {
    // Open the mic concurrently with the bearer + connect handshake (TLS +
    // WebSocket + `transcript.created`). Both legs take hundreds of ms and used
    // to run in series before any capture, clipping the first word of a hold.
    let (mic_tx, mic_rx) = mpsc::channel::<Vec<u8>>(64);
    let sample_rate = config.sample_rate;
    // `spawn_pcm_capture` blocks until the device opens; keep it off the runtime.
    let capture_task =
        tokio::task::spawn_blocking(move || crate::audio::spawn_pcm_capture(sample_rate, mic_tx));

    // Drain mic before connect resolves so capture never backpressures while
    // the socket comes up.
    let (audio_tx_tx, audio_tx_rx) = tokio::sync::oneshot::channel::<mpsc::Sender<Vec<u8>>>();
    tokio::spawn(forward_pcm(mic_rx, audio_tx_rx));

    let connect = async {
        let bearer = crate::auth::require_bearer(auth).await?;
        StreamingSttSession::connect(config, &bearer).await
    };
    let (connect_res, capture_res) = tokio::join!(connect, capture_task);

    // Resolve the mic first so a device/permission failure wins over a socket
    // error; the `?` on `connect_res` then drops `capture`, releasing the mic.
    let capture = match capture_res {
        Ok(Ok(handle)) => handle,
        Ok(Err(e)) => return Err(e),
        Err(join_err) => {
            return Err(VoiceError::Config(format!(
                "voice capture task failed: {join_err}"
            )));
        }
    };
    let mut stt = connect_res?;

    // Hand the live sender to the forwarder; it flushes the backlog then streams.
    let audio_tx = stt
        .audio_sender()
        .ok_or_else(|| VoiceError::Stt("STT audio sender unavailable".into()))?;
    let _ = audio_tx_tx.send(audio_tx);

    let (finish_tx, mut finish_rx) = mpsc::channel::<()>(1);

    let mut capture = Some(capture);
    let out = event_tx.clone();
    let reader = tokio::spawn(async move {
        // Stop the mic (releasing the device and dropping the capture thread's
        // clone of the audio sender) before signalling end-of-utterance, so no
        // stray PCM is queued after `audio.done`. Idempotent via `Option::take`.
        let stop_capture = |capture: &mut Option<crate::audio::CaptureHandle>| {
            if let Some(handle) = capture.take() {
                handle.stop();
            }
        };
        // No transcript within the timeout → tear down. Disarmed after speech.
        let no_speech_deadline = tokio::time::Instant::now() + NO_SPEECH_TIMEOUT;
        let mut awaiting_speech = true;
        // Chunk-final (`is_final && !speech_final`) text is locked: the server
        // sends it as a delta of the turn. Stitch those deltas into the live
        // preview so a long pauseless utterance keeps accumulating on screen
        // instead of resetting to the latest ~3s chunk. The committed prompt
        // text never comes from here — only from `speech_final`, which the
        // server produces as a clean one-pass re-transcription of the whole
        // turn (better than stitched deltas). Reset on each `speech_final`.
        let mut locked_prefix = String::new();
        loop {
            tokio::select! {
                msg = finish_rx.recv() => {
                    if msg.is_some() {
                        // User ended the turn; stop the no-speech watchdog.
                        awaiting_speech = false;
                        stop_capture(&mut capture);
                        stt.finish_audio();
                    } else {
                        return;
                    }
                }
                _ = tokio::time::sleep_until(no_speech_deadline), if awaiting_speech => {
                    // Tear down rather than streaming a dead mic until the user stops.
                    stop_capture(&mut capture);
                    stt.finish_audio();
                    let (message, hint) = no_speech_error();
                    let _ = out.send(VoiceEvent::Error { message, hint }).await;
                    return;
                }
                ev = stt.recv() => {
                    match ev {
                        Some(StreamingSttEvent::Partial(p)) => {
                            let text = p.text.trim();
                            if text.is_empty() {
                                continue;
                            }
                            // Real speech arrived: disarm the no-speech watchdog.
                            awaiting_speech = false;

                            let event = if p.speech_final {
                                locked_prefix.clear();
                                VoiceEvent::UtteranceFinal { text: p.text }
                            } else if p.is_final {
                                // Lock this chunk's delta into the running preview.
                                if !locked_prefix.is_empty() {
                                    locked_prefix.push(' ');
                                }
                                locked_prefix.push_str(text);
                                VoiceEvent::InterimTranscript {
                                    text: locked_prefix.clone(),
                                }
                            } else if locked_prefix.is_empty() {
                                VoiceEvent::InterimTranscript {
                                    text: text.to_owned(),
                                }
                            } else {
                                VoiceEvent::InterimTranscript {
                                    text: format!("{locked_prefix} {text}"),
                                }
                            };
                            // Receiver gone (pager dropped the channel): tear down.
                            if out.send(event).await.is_err() {
                                return;
                            }
                        }
                        Some(StreamingSttEvent::Done { text }) => {
                            locked_prefix.clear();
                            if !text.trim().is_empty() {
                                awaiting_speech = false;
                                let _ = out.send(VoiceEvent::UtteranceFinal { text }).await;
                            }
                        }
                        Some(StreamingSttEvent::Error { message }) => {
                            let _ = out.send(VoiceEvent::Error { message, hint: None }).await;
                            return;
                        }
                        Some(StreamingSttEvent::Ready) | None => return,
                    }
                }
            }
        }
    });

    Ok(ActivePtt { finish_tx, reader })
}

#[cfg(all(test, feature = "audio"))]
mod tests {
    use super::*;

    /// Chunks captured before the STT sender arrives are flushed (in order)
    /// ahead of the live stream, with nothing reordered or dropped across the
    /// handoff.
    #[tokio::test]
    async fn forward_pcm_delivers_buffered_then_live_in_order() {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<u8>>(8);
        let (tx_tx, tx_rx) = tokio::sync::oneshot::channel();
        let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<u8>>(8);
        let task = tokio::spawn(forward_pcm(mic_rx, tx_rx));

        // Buffered before the live sender is handed over, then flushed once it
        // arrives. (Keep `mic_tx` open across the handoff: a mic that closes
        // before the socket is ready means "abandoned", and discards the
        // backlog — see the separate test.)
        mic_tx.send(vec![1]).await.unwrap();
        mic_tx.send(vec![2]).await.unwrap();
        tx_tx.send(audio_tx).unwrap();
        assert_eq!(audio_rx.recv().await, Some(vec![1]));
        assert_eq!(audio_rx.recv().await, Some(vec![2]));

        // Streamed live afterward, still in order.
        mic_tx.send(vec![3]).await.unwrap();
        assert_eq!(audio_rx.recv().await, Some(vec![3]));

        drop(mic_tx);
        assert_eq!(audio_rx.recv().await, None, "ends when the mic closes");
        task.await.unwrap();
    }

    /// Mic stops before the socket is ready → the forwarder exits cleanly.
    #[tokio::test]
    async fn forward_pcm_returns_when_mic_closes_before_connect() {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<u8>>(8);
        let (_tx_tx, tx_rx) = tokio::sync::oneshot::channel::<mpsc::Sender<Vec<u8>>>();
        let task = tokio::spawn(forward_pcm(mic_rx, tx_rx));
        drop(mic_tx);
        task.await.unwrap();
    }

    /// Connect fails (oneshot sender dropped without a value) → forwarder exits
    /// and the buffered audio is discarded.
    #[tokio::test]
    async fn forward_pcm_returns_when_connect_fails() {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<u8>>(8);
        let (tx_tx, tx_rx) = tokio::sync::oneshot::channel::<mpsc::Sender<Vec<u8>>>();
        let task = tokio::spawn(forward_pcm(mic_rx, tx_rx));
        mic_tx.send(vec![1]).await.unwrap();
        drop(tx_tx);
        task.await.unwrap();
    }

    #[test]
    fn no_speech_error_carries_permission_hint() {
        let (message, hint) = no_speech_error();
        assert_eq!(message, "No speech was detected. Voice stopped.");
        assert!(hint.is_some_and(|hint| hint.contains(crate::probe::mic_fix_help())));
    }
}
