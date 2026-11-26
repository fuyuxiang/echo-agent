//! PCM16 mono capture via cpal for streaming STT.
//!
//! Prefers a device input config that natively supports the target rate (16 kHz)
//! so no resampling is needed; otherwise it uses the device default (typically
//! 48 kHz stereo F32 on macOS) and downmixes + resamples to 16 kHz mono for the
//! STT API. cpal streams are not `Send` on all platforms; capture runs on a
//! dedicated std thread and forwards PCM chunks through a sync channel.
//!
//! # Two roles: in-process backend and `__mic-capture` child
//!
//! On Windows this module is the capture backend itself (WASAPI's in-process
//! memory cost is modest). On macOS, opening CoreAudio in-process permanently
//! dirties several MB that the OS never returns after the stream drops, so
//! [`super::capture_subprocess`] re-execs the binary as a short-lived
//! `__mic-capture` helper instead; this module provides that child
//! ([`run_capture_child_cli`]) and the in-process fallback for when self-exec
//! is unavailable (e.g. the on-disk binary was replaced by an update).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::TrySendError;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use tokio::sync::mpsc as async_mpsc;

use crate::error::VoiceError;

/// Stop handle for the cpal input stream (owned by a background thread).
pub struct CaptureHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    bridge: tokio::task::JoinHandle<()>,
}

impl CaptureHandle {
    /// Stop capture and wait for the thread to exit.
    ///
    /// Dropping a `CaptureHandle` also stops capture (see the `Drop` impl), but
    /// without joining; call `stop()` when you need to be sure the device is
    /// released before continuing.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // `Drop` runs next and aborts the bridge task.
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        // Always signal the capture thread to exit so the mic is released even
        // when `stop()` was never called — e.g. the STT session ended on its
        // own (server close / error) or the pipeline shut down mid-utterance.
        // The thread observes the flag within one poll interval and exits,
        // dropping the cpal stream. We deliberately do not join here so `Drop`
        // never blocks (it may run on an async executor).
        self.stop.store(true, Ordering::Release);
        self.bridge.abort();
    }
}

/// Spawn cpal capture; PCM16 LE chunks are forwarded to `pcm_tx`.
pub fn spawn_pcm_capture(
    sample_rate: u32,
    pcm_tx: async_mpsc::Sender<Vec<u8>>,
) -> Result<CaptureHandle, VoiceError> {
    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
    // Bridge the cpal callback's std sync channel to the async STT sender. The
    // `recv()` blocks between audio chunks for the whole session, so it runs on
    // the blocking pool (via `spawn_blocking` + `blocking_send`) instead of a
    // core runtime worker — parking a worker here would shrink executor capacity
    // under pager load. The loop exits on its own when capture stops (sync_tx is
    // dropped) or the STT consumer goes away (`blocking_send` errors), so the
    // `abort()` in `CaptureHandle`'s teardown is only a backstop.
    let bridge = tokio::task::spawn_blocking(move || {
        while let Ok(bytes) = sync_rx.recv() {
            if pcm_tx.blocking_send(bytes).is_err() {
                break;
            }
        }
    });

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), VoiceError>>(1);
    let thread = thread::spawn(move || {
        run_capture_loop(sample_rate, sync_tx, stop_flag, ready_tx);
    });

    // Wait briefly for the device to actually open (mirrors the STT
    // `wait_ready` handshake) so device/permission failures propagate to the
    // caller — and on to a `VoiceEvent::Error` toast — instead of leaving the
    // session "listening" with no audio.
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = thread.join();
            bridge.abort();
            return Err(e);
        }
        Err(_) => {
            stop.store(true, Ordering::Release);
            let _ = thread.join();
            bridge.abort();
            return Err(VoiceError::Config(
                "voice capture did not start within 5s".into(),
            ));
        }
    }

    Ok(CaptureHandle {
        stop,
        thread: Some(thread),
        bridge,
    })
}

/// Default cpal input device, or a config error when the host has none.
fn default_input_device() -> Result<cpal::Device, VoiceError> {
    cpal::default_host()
        .default_input_device()
        .ok_or_else(|| VoiceError::Config("no default input audio device".into()))
}

/// Default input device without opening a stream ([`crate::probe::input_device_info`]).
pub fn input_device_info() -> Result<crate::probe::InputDeviceInfo, VoiceError> {
    let device = default_input_device()?;
    let name = device.name().unwrap_or_else(|_| "<unknown>".to_string());
    let detail = match device.default_input_config() {
        Ok(c) => format!(
            "{} Hz, {} ch, {:?}",
            c.sample_rate().0,
            c.channels(),
            c.sample_format()
        ),
        Err(e) => format!("default config unavailable: {e}"),
    };
    Ok(crate::probe::InputDeviceInfo { name, detail })
}

/// Record mono PCM16 LE for a fixed duration (probe / diagnostics).
pub fn capture_pcm_for_duration(
    sample_rate: u32,
    seconds: u32,
) -> Result<(Vec<u8>, u32), VoiceError> {
    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), VoiceError>>(1);
    let thread = thread::spawn(move || {
        run_capture_loop(sample_rate, sync_tx, stop_flag, ready_tx);
    });

    // Surface device-open failures before recording instead of returning empty.
    match ready_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = thread.join();
            return Err(e);
        }
        Err(_) => {
            stop.store(true, Ordering::Release);
            let _ = thread.join();
            return Err(VoiceError::Config(
                "voice capture did not start within 2s".into(),
            ));
        }
    }

    thread::sleep(Duration::from_secs(seconds.max(1) as u64));
    stop.store(true, Ordering::Release);
    let _ = thread.join();

    let mut pcm = Vec::new();
    let mut chunks = 0u32;
    while let Ok(chunk) = sync_rx.try_recv() {
        chunks += 1;
        pcm.extend_from_slice(&chunk);
    }
    Ok((pcm, chunks))
}

struct CaptureStreamParams<'a> {
    device: &'a cpal::Device,
    stream_config: cpal::StreamConfig,
    in_channels: u16,
    stream_rate: u32,
    target_rate: u32,
    sync_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    /// Count of PCM chunks dropped because the channel was full. Logged off the
    /// audio thread by `run_capture_loop`.
    dropped: Arc<AtomicUsize>,
}

fn run_capture_loop(
    sample_rate: u32,
    sync_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    ready_tx: std::sync::mpsc::SyncSender<Result<(), VoiceError>>,
) {
    let dropped = Arc::new(AtomicUsize::new(0));
    // Open the device first and report success/failure to the caller (the
    // capture-side equivalent of the STT `wait_ready` handshake) so that
    // device/permission errors surface as a `VoiceError` instead of being
    // logged silently here.
    let (stream, device_name) = match open_capture_stream(
        sample_rate,
        sync_tx,
        Arc::clone(&stop),
        Arc::clone(&dropped),
    ) {
        Ok(v) => {
            let _ = ready_tx.send(Ok(()));
            v
        }
        Err(e) => {
            tracing::warn!(error = %e, "voice capture failed to start");
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    run_capture_poll_loop(stream, stop, dropped, device_name);
}

/// Open the input device, build, and start the cpal capture stream. All
/// device/config/permission failures surface here as a `VoiceError` so the
/// caller can report them before entering the steady-state loop.
pub(super) fn open_capture_stream(
    sample_rate: u32,
    sync_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
) -> Result<(cpal::Stream, String), VoiceError> {
    let device = default_input_device()?;

    let device_name = device.name().unwrap_or_else(|_| "<unknown>".to_string());

    let default_config = device.default_input_config().map_err(|e| {
        VoiceError::Config(format!(
            "default input config for {device_name}: {e} (grant mic permission in System Settings)"
        ))
    })?;

    // Prefer a device-native `sample_rate` (e.g. a mic that supports 16 kHz
    // directly) so we can skip resampling entirely; fall back to the device
    // default and the linear resampler when no native config matches.
    let supported = native_rate_config(&device, sample_rate).unwrap_or(default_config);

    let stream_rate = supported.sample_rate().0;
    let in_channels = supported.channels();
    let sample_format = supported.sample_format();
    let stream_config: cpal::StreamConfig = supported.into();

    tracing::info!(
        device = %device_name,
        stream_rate,
        channels = in_channels,
        ?sample_format,
        target_rate = sample_rate,
        "voice capture stream"
    );

    let params = CaptureStreamParams {
        device: &device,
        stream_config,
        in_channels,
        stream_rate,
        target_rate: sample_rate,
        sync_tx,
        stop,
        dropped,
    };

    let stream = match sample_format {
        SampleFormat::F32 => build_capture_stream::<f32>(params)?,
        SampleFormat::F64 => build_capture_stream::<f64>(params)?,
        SampleFormat::I8 => build_capture_stream::<i8>(params)?,
        SampleFormat::I16 => build_capture_stream::<i16>(params)?,
        SampleFormat::I32 => build_capture_stream::<i32>(params)?,
        SampleFormat::I64 => build_capture_stream::<i64>(params)?,
        SampleFormat::U8 => build_capture_stream::<u8>(params)?,
        SampleFormat::U16 => build_capture_stream::<u16>(params)?,
        SampleFormat::U32 => build_capture_stream::<u32>(params)?,
        SampleFormat::U64 => build_capture_stream::<u64>(params)?,
        other => {
            return Err(VoiceError::Config(format!(
                "unsupported input sample format {other:?} on {device_name} \
                 (supported: f32/f64/i8/i16/i32/i64/u8/u16/u32/u64)"
            )));
        }
    };

    stream
        .play()
        .map_err(|e| VoiceError::Config(format!("play input stream: {e}")))?;

    Ok((stream, device_name))
}

/// Steady-state loop: wait for shutdown and report dropped frames off the
/// real-time audio thread (logging here keeps the callback allocation/lock-free).
fn run_capture_poll_loop(
    stream: cpal::Stream,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
    device_name: String,
) {
    let mut last_reported = 0usize;
    let mut ticks: u32 = 0;
    while !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(50));
        ticks += 1;
        // ~once per second
        if ticks.is_multiple_of(20) {
            let total = dropped.load(Ordering::Relaxed);
            if total > last_reported {
                tracing::warn!(
                    device = %device_name,
                    dropped_total = total,
                    dropped_since_last = total - last_reported,
                    "voice capture dropping PCM chunks (consumer not keeping up)"
                );
                last_reported = total;
            }
        }
    }

    drop(stream);
    let total = dropped.load(Ordering::Relaxed);
    if total > 0 {
        tracing::warn!(
            device = %device_name,
            dropped_total = total,
            "voice capture finished with dropped PCM chunks"
        );
    }
}

/// Find a supported input config whose range includes `target_rate`, so capture
/// runs at the STT rate with no resampling. Prefers a config matching the
/// device's default sample format; returns `None` when nothing matches.
fn native_rate_config(
    device: &cpal::Device,
    target_rate: u32,
) -> Option<cpal::SupportedStreamConfig> {
    let target = cpal::SampleRate(target_rate);
    let preferred_format = device
        .default_input_config()
        .ok()
        .map(|c| c.sample_format());

    let configs: Vec<_> = device.supported_input_configs().ok()?.collect();
    let contains_target = |c: &cpal::SupportedStreamConfigRange| {
        c.min_sample_rate() <= target && target <= c.max_sample_rate()
    };

    configs
        .iter()
        .find(|c| contains_target(c) && Some(c.sample_format()) == preferred_format)
        .or_else(|| configs.iter().find(|c| contains_target(c)))
        .map(|c| c.with_sample_rate(target))
}

fn build_capture_stream<T>(params: CaptureStreamParams<'_>) -> Result<cpal::Stream, VoiceError>
where
    T: Sample + SizedSample,
    i16: FromSample<T>,
{
    let CaptureStreamParams {
        device,
        stream_config,
        in_channels,
        stream_rate,
        target_rate,
        sync_tx,
        stop,
        dropped,
    } = params;
    let channels = in_channels as usize;
    let stop_cb = Arc::clone(&stop);

    let stream = device
        .build_input_stream(
            &stream_config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                if stop_cb.load(Ordering::Acquire) {
                    return;
                }
                let mono = frames_to_mono_i16(data, channels);
                let pcm = if stream_rate == target_rate {
                    mono
                } else {
                    resample_mono_i16(&mono, stream_rate, target_rate)
                };
                if pcm.is_empty() {
                    return;
                }
                send_pcm(&pcm, &sync_tx, &dropped);
            },
            |err| {
                tracing::warn!(error = %err, "voice capture stream error");
            },
            None,
        )
        .map_err(|e| VoiceError::Config(format!("build input stream: {e}")))?;

    Ok(stream)
}

/// Non-blocking send from the real-time audio callback: shed load (and count
/// it) rather than ever blocking the device thread.
fn send_pcm(pcm: &[i16], sync_tx: &std::sync::mpsc::SyncSender<Vec<u8>>, dropped: &AtomicUsize) {
    let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
    match sync_tx.try_send(bytes) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

fn frames_to_mono_i16<T>(data: &[T], channels: usize) -> Vec<i16>
where
    T: Sample,
    i16: FromSample<T>,
{
    if channels == 0 {
        return Vec::new();
    }
    if channels == 1 {
        return data.iter().map(|s| i16::from_sample(*s)).collect();
    }
    let mut mono = Vec::with_capacity(data.len() / channels);
    for frame in data.chunks_exact(channels) {
        let mut sum: i32 = 0;
        for sample in frame {
            sum += i16::from_sample(*sample) as i32;
        }
        let avg = (sum / channels as i32).clamp(i16::MIN as i32, i16::MAX as i32);
        mono.push(avg as i16);
    }
    mono
}

fn resample_mono_i16(samples: &[i16], input_rate: u32, output_rate: u32) -> Vec<i16> {
    if samples.is_empty() || input_rate == 0 || output_rate == 0 {
        return Vec::new();
    }
    if input_rate == output_rate {
        return samples.to_vec();
    }

    let output_len =
        ((samples.len() as u64 * output_rate as u64) / input_rate as u64).max(1) as usize;
    let step = input_rate as f64 / output_rate as f64;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let src_pos = i as f64 * step;
        let idx = src_pos.floor() as usize;
        let frac = src_pos - idx as f64;
        let s0 = samples[idx] as f64;
        let s1 = *samples.get(idx + 1).unwrap_or(&samples[idx]) as f64;
        let sample = s0 + (s1 - s0) * frac;
        let clamped = sample.round().max(i16::MIN as f64).min(i16::MAX as f64);
        output.push(clamped as i16);
    }

    output
}

// ---------------------------------------------------------------------------
// `__mic-capture` child mode (see the module docs and `capture_subprocess`).
// ---------------------------------------------------------------------------

/// Run the `__mic-capture` helper child. `args` is argv after the subcommand:
/// `--rate <N>` streams PCM16 mono LE at `N` Hz to stdout; `--device-info`
/// prints the default input device instead (one line, no stream opened).
///
/// Wire protocol (stdout): one status header line, then raw PCM.
/// - `READY <device>\n` followed by the PCM byte stream, or
/// - `INFO <name>\t<detail>\n` for `--device-info`, or
/// - `ERR <message>\n` and a non-zero exit on any failure.
///
/// The child exits when its stdout write fails (parent closed the pipe or
/// died) or when the parent kills it — it never outlives the capture session.
pub(crate) fn run_capture_child_cli(args: Vec<String>) -> i32 {
    // Route the child's tracing (device open info, cpal warnings) to stderr,
    // which the parent drains into its debug log — plain text, since the
    // reader is a pipe, not a terminal. Stdout is the protocol channel and
    // must stay clean.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .try_init();

    match parse_child_args(&args) {
        Ok(ChildMode::DeviceInfo) => run_device_info_child(),
        Ok(ChildMode::Capture { rate }) => run_capture_child(rate),
        Err(msg) => {
            emit_header(&super::protocol::err_line(&msg));
            2
        }
    }
}

/// Write a header line to stdout without panicking: `println!` aborts on
/// EPIPE, and a helper whose parent died must exit quietly, not crash.
fn emit_header(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// What the helper child was asked to do (parsed from its argv).
#[derive(Debug, PartialEq, Eq)]
enum ChildMode {
    Capture { rate: u32 },
    DeviceInfo,
}

/// Parse the helper argv. Pure so the contract is unit-testable.
fn parse_child_args(args: &[String]) -> Result<ChildMode, String> {
    let mut rate: u32 = crate::config::DEFAULT_SAMPLE_RATE;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--device-info" => return Ok(ChildMode::DeviceInfo),
            "--rate" => {
                i += 1;
                rate = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .filter(|r| *r > 0)
                    .ok_or_else(|| "bad --rate".to_string())?;
            }
            other => return Err(format!("unknown mic-capture arg: {other}")),
        }
        i += 1;
    }
    Ok(ChildMode::Capture { rate })
}

fn run_device_info_child() -> i32 {
    match input_device_info() {
        Ok(info) => {
            emit_header(&super::protocol::info_line(&info.name, &info.detail));
            0
        }
        Err(e) => {
            emit_header(&super::protocol::err_line(&e.to_string()));
            1
        }
    }
}

fn run_capture_child(rate: u32) -> i32 {
    use std::io::Write;

    let (sync_tx, sync_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
    let stop = Arc::new(AtomicBool::new(false));
    let stream = match open_capture_stream(
        rate,
        sync_tx,
        Arc::clone(&stop),
        Arc::new(AtomicUsize::new(0)),
    ) {
        Ok((stream, device_name)) => {
            emit_header(&super::protocol::ready_line(&device_name));
            stream
        }
        Err(e) => {
            emit_header(&super::protocol::err_line(&e.to_string()));
            return 1;
        }
    };

    let mut out = std::io::stdout().lock();
    // Flush per chunk: chunks are small (~10 ms of PCM) and streaming STT
    // wants them promptly, not batched by the stdout buffer.
    loop {
        match sync_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(chunk) => {
                if out.write_all(&chunk).and_then(|()| out.flush()).is_err() {
                    break; // parent closed the pipe / died → stop capturing
                }
            }
            // A silent device produces no writes, so parent death would go
            // unnoticed and orphan this child; poll for reparenting (the
            // parent normally kills us long before this fires).
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                #[cfg(unix)]
                if std::os::unix::process::parent_id() == 1 {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop.store(true, Ordering::Release);
    drop(stream);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_args_default_to_capture_at_default_rate() {
        assert_eq!(
            parse_child_args(&[]),
            Ok(ChildMode::Capture {
                rate: crate::config::DEFAULT_SAMPLE_RATE
            })
        );
        let args = vec!["--rate".to_string(), "24000".to_string()];
        assert_eq!(
            parse_child_args(&args),
            Ok(ChildMode::Capture { rate: 24000 })
        );
    }

    #[test]
    fn child_args_reject_bad_rate_and_unknown_flags() {
        assert!(parse_child_args(&["--rate".to_string()]).is_err());
        assert!(parse_child_args(&["--rate".to_string(), "0".to_string()]).is_err());
        assert!(parse_child_args(&["--rate".to_string(), "x".to_string()]).is_err());
        assert!(parse_child_args(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn child_args_device_info_wins() {
        assert_eq!(
            parse_child_args(&["--device-info".to_string()]),
            Ok(ChildMode::DeviceInfo)
        );
    }

    #[test]
    fn send_pcm_counts_shed_chunks() {
        let (tx, _rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let dropped = AtomicUsize::new(0);

        send_pcm(&[100], &tx, &dropped); // fills the channel
        send_pcm(&[200], &tx, &dropped); // shed
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn resample_halves_rate() {
        let input: Vec<i16> = (0..48).map(|i| (i * 100) as i16).collect();
        let out = resample_mono_i16(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16);
    }

    #[test]
    fn downmix_stereo_to_mono() {
        let stereo = [i16::MAX, i16::MIN];
        let mono = frames_to_mono_i16(&stereo, 2);
        assert_eq!(mono.len(), 1);
        assert_eq!(mono[0], 0);
    }
}
