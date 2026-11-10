//! Native system-output capture for meeting recordings. Microphone-only
//! recording keeps the WebView path (and its echo cancellation); system and
//! combined recording use the platform's Core Audio/WASAPI loopback stream.

use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    meeting_id: String,
    active: bool,
    paused: bool,
    level: f32,
    recorded_samples: u64,
    error: Option<String>,
    pending_save: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSupport {
    system_audio: bool,
    detail: Option<&'static str>,
}

#[tauri::command]
pub fn meeting_capture_support(app: tauri::AppHandle) -> CaptureSupport {
    crate::meeting_minutes::preload_speech(&app);
    capture_support()
}

fn capture_support() -> CaptureSupport {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        #[cfg(target_os = "macos")]
        let supported = std::process::Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .and_then(|version| {
                let mut parts = version.trim().split('.');
                Some((
                    parts.next()?.parse::<u32>().ok()?,
                    parts.next()?.parse::<u32>().ok()?,
                ))
            })
            .is_some_and(|(major, minor)| major > 14 || major == 14 && minor >= 6);
        #[cfg(target_os = "windows")]
        let supported = true;
        CaptureSupport {
            system_audio: supported,
            detail: if cfg!(target_os = "macos") {
                Some(if supported {
                    "首次使用需允许系统音频录制"
                } else {
                    "系统声音采集需要 macOS 14.6 或更高版本"
                })
            } else {
                Some("录制当前默认播放设备的声音")
            },
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        CaptureSupport {
            system_audio: false,
            detail: Some("当前平台暂不支持系统声音采集"),
        }
    }
}

#[tauri::command]
pub async fn meeting_capture_start(
    meeting_id: String,
    mode: String,
) -> Result<CaptureStatus, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        if !capture_support().system_audio {
            return Err("当前系统版本不支持系统声音采集".into());
        }
        tokio::task::spawn_blocking(move || native::start(meeting_id, mode))
            .await
            .map_err(|error| format!("启动录音任务失败：{error}"))?
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (meeting_id, mode);
        Err("当前平台暂不支持系统声音采集".into())
    }
}

#[tauri::command]
pub fn meeting_capture_status() -> Option<CaptureStatus> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        native::status()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[tauri::command]
pub async fn meeting_capture_pause(
    meeting_id: String,
    paused: bool,
) -> Result<crate::meeting_minutes::MeetingRecord, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        tokio::task::spawn_blocking(move || native::pause(&meeting_id, paused))
            .await
            .map_err(|error| format!("切换录音状态失败：{error}"))?
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (meeting_id, paused);
        Err("当前平台暂不支持系统声音采集".into())
    }
}

#[tauri::command]
pub async fn meeting_capture_stop(
    meeting_id: String,
) -> Result<crate::meeting_minutes::MeetingRecord, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        tokio::task::spawn_blocking(move || native::stop(&meeting_id))
            .await
            .map_err(|error| format!("结束录音任务失败：{error}"))?
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = meeting_id;
        Err("当前平台暂不支持系统声音采集".into())
    }
}

/// Finalize any native recording before a controlled exit or updater restart.
/// Failed writes retain the capture and its PCM buffer for an explicit retry.
pub(crate) async fn finish_before_exit() -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let Some(capture) = native::status() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || native::stop(&capture.meeting_id))
            .await
            .map_err(|error| format!("退出前保存录音任务失败：{error}"))?
            .map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(())
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::CaptureStatus;
    use crate::meeting_minutes::{
        meeting_append_pcm, meeting_finish_recording, meeting_get, meeting_set_paused,
        MeetingRecord, MAX_RECORDING_SAMPLES,
    };
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{
        Data, FromSample, Sample, SampleFormat, SizedSample, Stream, SupportedStreamConfig,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    const RATE: usize = 16_000;
    const TICK_SAMPLES: usize = RATE / 10;
    const QUEUE_LIMIT: usize = RATE;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Source {
        System,
        Microphone,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        System,
        Both,
    }

    impl Mode {
        fn parse(raw: &str) -> Result<Self, String> {
            match raw {
                "system" => Ok(Self::System),
                "both" => Ok(Self::Both),
                _ => Err("录音来源无效".into()),
            }
        }
    }

    enum Message {
        Audio(Source, Vec<f32>),
        Pause(bool, SyncSender<Result<MeetingRecord, String>>),
        Stop(SyncSender<Result<MeetingRecord, String>>),
    }

    struct ActiveCapture {
        meeting_id: String,
        sender: SyncSender<Message>,
        accepting: Arc<AtomicBool>,
        status: Arc<Mutex<CaptureStatus>>,
        worker: JoinHandle<()>,
    }

    static ACTIVE: OnceLock<Mutex<Option<ActiveCapture>>> = OnceLock::new();

    fn active() -> &'static Mutex<Option<ActiveCapture>> {
        ACTIVE.get_or_init(|| Mutex::new(None))
    }

    pub fn status() -> Option<CaptureStatus> {
        let guard = active().lock().ok()?;
        guard
            .as_ref()?
            .status
            .lock()
            .ok()
            .map(|status| status.clone())
    }

    pub fn start(meeting_id: String, raw_mode: String) -> Result<CaptureStatus, String> {
        let mode = Mode::parse(&raw_mode)?;
        let record = meeting_get(meeting_id.clone())?;
        if record.status != "recording" || record.recorded_samples != 0 {
            return Err("此会议无法开始新的录音".into());
        }
        if record.capture_source.as_deref() != Some(raw_mode.as_str()) {
            return Err("会议录音来源不匹配".into());
        }
        let mut guard = active().lock().map_err(|_| "录音状态不可用".to_string())?;
        if guard.is_some() {
            return Err("已有会议正在录音，请先结束当前录音".into());
        }
        let status = Arc::new(Mutex::new(CaptureStatus {
            meeting_id: meeting_id.clone(),
            active: false,
            paused: false,
            level: 0.0,
            recorded_samples: 0,
            error: None,
            pending_save: false,
        }));
        let accepting = Arc::new(AtomicBool::new(true));
        // Bound the callback queue so a slow disk can never grow audio memory without limit.
        let (sender, receiver) = mpsc::sync_channel(64);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_status = Arc::clone(&status);
        let worker_accepting = Arc::clone(&accepting);
        let worker_sender = sender.clone();
        let worker_id = meeting_id.clone();
        let worker = thread::Builder::new()
            .name("meeting-audio-capture".into())
            .spawn(move || {
                run_worker(
                    worker_id,
                    mode,
                    receiver,
                    worker_sender,
                    worker_status,
                    worker_accepting,
                    ready_tx,
                )
            })
            .map_err(|error| format!("创建录音线程失败：{error}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                let snapshot = status
                    .lock()
                    .map_err(|_| "录音状态不可用".to_string())?
                    .clone();
                *guard = Some(ActiveCapture {
                    meeting_id,
                    sender,
                    accepting,
                    status,
                    worker,
                });
                Ok(snapshot)
            }
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err("音频采集初始化意外中断".into())
            }
        }
    }

    pub fn pause(meeting_id: &str, paused: bool) -> Result<MeetingRecord, String> {
        let guard = active().lock().map_err(|_| "录音状态不可用".to_string())?;
        let handle = guard.as_ref().ok_or("当前没有正在进行的系统声音录音")?;
        if handle.meeting_id != meeting_id {
            return Err("会议录音 ID 不匹配".into());
        }
        if paused {
            handle.accepting.store(false, Ordering::Release);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        handle
            .sender
            .send(Message::Pause(paused, reply_tx))
            .map_err(|_| "录音线程已退出".to_string())?;
        reply_rx.recv().map_err(|_| "录音线程已退出".to_string())?
    }

    pub fn stop(meeting_id: &str) -> Result<MeetingRecord, String> {
        let mut guard = active().lock().map_err(|_| "录音状态不可用".to_string())?;
        let handle = guard.as_ref().ok_or("当前没有对应的系统声音录音")?;
        if handle.meeting_id != meeting_id {
            return Err("会议录音 ID 不匹配".into());
        }
        handle.accepting.store(false, Ordering::Release);
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let result = handle
            .sender
            .send(Message::Stop(reply_tx))
            .map_err(|_| "录音线程已退出".to_string())
            .and_then(|()| reply_rx.recv().map_err(|_| "录音线程已退出".to_string()))
            .and_then(|result| result);
        // On a write failure the worker retains its pending samples. Keep its
        // handle so a later Stop retries them and Start cannot replace them.
        if result.is_ok() || handle.worker.is_finished() {
            if let Some(handle) = guard.take() {
                let _ = handle.worker.join();
            }
        }
        result
    }

    struct Resampler {
        input_rate: u32,
        channels: usize,
        phase: u64,
        sum: f32,
        count: u32,
    }

    impl Resampler {
        fn new(config: &SupportedStreamConfig) -> Self {
            Self {
                input_rate: config.sample_rate(),
                channels: config.channels() as usize,
                phase: 0,
                sum: 0.0,
                count: 0,
            }
        }

        fn consume<T>(&mut self, input: &[T]) -> Vec<f32>
        where
            T: SizedSample,
            f32: FromSample<T>,
        {
            let mut result = Vec::with_capacity(
                input.len() * RATE / (self.channels.max(1) * self.input_rate as usize) + 1,
            );
            for frame in input.chunks_exact(self.channels.max(1)) {
                let mono = frame
                    .iter()
                    .map(|sample| f32::from_sample(*sample))
                    .sum::<f32>()
                    / self.channels.max(1) as f32;
                self.sum += mono;
                self.count += 1;
                self.phase += RATE as u64;
                while self.phase >= self.input_rate as u64 {
                    result.push(
                        (if self.count == 0 {
                            mono
                        } else {
                            self.sum / self.count as f32
                        })
                        .clamp(-1.0, 1.0),
                    );
                    self.phase -= self.input_rate as u64;
                    self.sum = 0.0;
                    self.count = 0;
                }
            }
            result
        }

        fn convert(&mut self, data: &Data) -> Vec<f32> {
            match data.sample_format() {
                SampleFormat::F32 => self.consume(data.as_slice::<f32>().unwrap_or(&[])),
                SampleFormat::F64 => self.consume(data.as_slice::<f64>().unwrap_or(&[])),
                SampleFormat::I16 => self.consume(data.as_slice::<i16>().unwrap_or(&[])),
                SampleFormat::I24 => self.consume(data.as_slice::<cpal::I24>().unwrap_or(&[])),
                SampleFormat::I32 => self.consume(data.as_slice::<i32>().unwrap_or(&[])),
                SampleFormat::U8 => self.consume(data.as_slice::<u8>().unwrap_or(&[])),
                SampleFormat::U16 => self.consume(data.as_slice::<u16>().unwrap_or(&[])),
                SampleFormat::U24 => self.consume(data.as_slice::<cpal::U24>().unwrap_or(&[])),
                SampleFormat::U32 => self.consume(data.as_slice::<u32>().unwrap_or(&[])),
                _ => Vec::new(),
            }
        }
    }

    fn stream_for(
        device: &cpal::Device,
        config: SupportedStreamConfig,
        source: Source,
        sender: SyncSender<Message>,
        status: Arc<Mutex<CaptureStatus>>,
        accepting: Arc<AtomicBool>,
    ) -> Result<Stream, String> {
        if !matches!(
            config.sample_format(),
            SampleFormat::F32
                | SampleFormat::F64
                | SampleFormat::I16
                | SampleFormat::I24
                | SampleFormat::I32
                | SampleFormat::U8
                | SampleFormat::U16
                | SampleFormat::U24
                | SampleFormat::U32
        ) {
            return Err("当前音频设备使用了不支持的采样格式".into());
        }
        let mut resampler = Resampler::new(&config);
        let overflow_status = Arc::clone(&status);
        let overflow_accepting = Arc::clone(&accepting);
        let error_status = Arc::clone(&status);
        let error_accepting = Arc::clone(&accepting);
        #[cfg(target_os = "macos")]
        if source == Source::System {
            cpal::force_loopback_for_next_input_stream();
        }
        let stream = device.build_input_stream_raw(
            config.config(), config.sample_format(),
            move |data, _| {
                if !accepting.load(Ordering::Acquire) {
                    resampler.phase = 0; resampler.sum = 0.0; resampler.count = 0;
                    return;
                }
                let samples = resampler.convert(data);
                if !samples.is_empty() {
                    if let Err(error) = sender.try_send(Message::Audio(source, samples)) {
                        overflow_accepting.store(false, Ordering::Release);
                        if let Ok(mut state) = overflow_status.lock() {
                            state.error = Some(format!("录音数据处理速度不足，已停止采集：{error}"));
                            state.active = false;
                        }
                    }
                }
            },
            move |error| {
                error_accepting.store(false, Ordering::Release);
                if let Ok(mut state) = error_status.lock() {
                    state.error = Some(format!("音频设备已中断：{error}"));
                    state.active = false;
                }
            }, None,
        ).map_err(|error| {
            if source == Source::System && cfg!(target_os = "macos") {
                format!("无法打开系统声音：{error}。请检查系统设置 > 隐私与安全性 > 系统音频录制中的授权")
            } else {
                format!("无法打开{}：{error}", if source == Source::System { "系统声音" } else { "麦克风" })
            }
        })?;
        Ok(stream)
    }

    fn push_bounded(queue: &mut VecDeque<f32>, samples: Vec<f32>) {
        queue.extend(samples);
        if queue.len() > QUEUE_LIMIT {
            queue.drain(..queue.len() - QUEUE_LIMIT);
        }
    }

    fn flush(
        meeting_id: &str,
        pending: &mut Vec<i16>,
        status: &Arc<Mutex<CaptureStatus>>,
    ) -> Result<MeetingRecord, String> {
        let record = if pending.is_empty() {
            meeting_get(meeting_id.to_string())?
        } else {
            let samples = std::mem::take(pending);
            match meeting_append_pcm(meeting_id.to_string(), samples.clone()) {
                Ok(record) => record,
                Err(error) => {
                    *pending = samples;
                    return Err(error);
                }
            }
        };
        if let Ok(mut state) = status.lock() {
            state.recorded_samples = record.recorded_samples;
        }
        Ok(record)
    }

    fn record_samples(
        mode: Mode,
        system: &mut VecDeque<f32>,
        microphone: &mut VecDeque<f32>,
        pending: &mut Vec<i16>,
        status: &Arc<Mutex<CaptureStatus>>,
        meeting_id: &str,
        count: usize,
    ) -> Result<(), String> {
        let mut peak = 0.0f32;
        for _ in 0..count {
            let output = system.pop_front().unwrap_or(0.0);
            let mixed = if mode == Mode::Both {
                (output * 0.8 + microphone.pop_front().unwrap_or(0.0) * 0.8).clamp(-1.0, 1.0)
            } else {
                output
            };
            peak = peak.max(mixed.abs());
            pending.push((mixed * 32767.0).round() as i16);
        }
        if let Ok(mut state) = status.lock() {
            state.level = (peak * 2.5).min(1.0);
        }
        if pending.len() >= RATE {
            flush(meeting_id, pending, status)?;
        }
        Ok(())
    }

    fn run_worker(
        meeting_id: String,
        mode: Mode,
        receiver: Receiver<Message>,
        sender: SyncSender<Message>,
        status: Arc<Mutex<CaptureStatus>>,
        accepting: Arc<AtomicBool>,
        ready: SyncSender<Result<(), String>>,
    ) {
        let setup = (|| -> Result<(Stream, Option<Stream>), String> {
            let host = cpal::default_host();
            let output = host.default_output_device().ok_or("未找到系统播放设备")?;
            let output_config = output
                .default_output_config()
                .map_err(|error| format!("无法读取系统播放设备：{error}"))?;
            let system_stream = stream_for(
                &output,
                output_config,
                Source::System,
                sender.clone(),
                Arc::clone(&status),
                Arc::clone(&accepting),
            )?;
            let microphone_stream = if mode == Mode::Both {
                let input = host.default_input_device().ok_or("未找到麦克风设备")?;
                let config = input
                    .default_input_config()
                    .map_err(|error| format!("无法读取麦克风设备：{error}"))?;
                Some(stream_for(
                    &input,
                    config,
                    Source::Microphone,
                    sender,
                    Arc::clone(&status),
                    Arc::clone(&accepting),
                )?)
            } else {
                None
            };
            system_stream
                .play()
                .map_err(|error| format!("无法开始采集系统声音：{error}"))?;
            if let Some(microphone) = &microphone_stream {
                microphone
                    .play()
                    .map_err(|error| format!("无法开始采集麦克风：{error}"))?;
            }
            Ok((system_stream, microphone_stream))
        })();
        let (system_stream, microphone_stream) = match setup {
            Ok(streams) => streams,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        if let Ok(mut state) = status.lock() {
            state.active = true;
        }
        let _ = ready.send(Ok(()));
        let mut system_stream = Some(system_stream);
        let mut microphone_stream = microphone_stream;
        let mut system = VecDeque::new();
        let mut microphone = VecDeque::new();
        let mut pending = Vec::with_capacity(RATE);
        let mut next_tick = Instant::now() + Duration::from_millis(100);
        let mut paused = false;
        loop {
            if next_tick <= Instant::now() {
                if !paused && accepting.load(Ordering::Acquire) {
                    if let Err(error) = record_samples(
                        mode,
                        &mut system,
                        &mut microphone,
                        &mut pending,
                        &status,
                        &meeting_id,
                        TICK_SAMPLES,
                    ) {
                        accepting.store(false, Ordering::Release);
                        if let Ok(mut state) = status.lock() {
                            state.error = Some(error);
                            state.active = false;
                        }
                    } else if status
                        .lock()
                        .is_ok_and(|state| state.recorded_samples >= MAX_RECORDING_SAMPLES)
                    {
                        accepting.store(false, Ordering::Release);
                        paused = true;
                        drop(system_stream.take());
                        drop(microphone_stream.take());
                        if let Ok(mut state) = status.lock() {
                            state.paused = true;
                            state.level = 0.0;
                        }
                    }
                }
                next_tick = Instant::now() + Duration::from_millis(100);
            }
            let timeout = next_tick.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(timeout) {
                Ok(Message::Audio(source, samples)) => {
                    if paused {
                        continue;
                    }
                    if source == Source::System {
                        push_bounded(&mut system, samples);
                    } else {
                        push_bounded(&mut microphone, samples);
                    }
                }
                Ok(Message::Pause(next, reply)) => {
                    let result = if next {
                        let tail = system.len().max(if mode == Mode::Both {
                            microphone.len()
                        } else {
                            0
                        });
                        let result = record_samples(
                            mode,
                            &mut system,
                            &mut microphone,
                            &mut pending,
                            &status,
                            &meeting_id,
                            tail,
                        )
                        .and_then(|_| flush(&meeting_id, &mut pending, &status))
                        .and_then(|_| meeting_set_paused(meeting_id.clone(), true));
                        paused = result.is_ok();
                        system.clear();
                        microphone.clear();
                        result
                    } else {
                        let failure = status.lock().ok().and_then(|state| state.error.clone());
                        let result = if let Some(error) = failure {
                            Err(error)
                        } else {
                            meeting_set_paused(meeting_id.clone(), false)
                        };
                        if result.is_ok() {
                            paused = false;
                            accepting.store(true, Ordering::Release);
                            next_tick = Instant::now() + Duration::from_millis(100);
                        }
                        result
                    };
                    if let Ok(mut state) = status.lock() {
                        state.paused = paused;
                        state.level = 0.0;
                    }
                    let _ = reply.send(result);
                }
                Ok(Message::Stop(reply)) => {
                    accepting.store(false, Ordering::Release);
                    drop(system_stream.take());
                    drop(microphone_stream.take());
                    let tail = system.len().max(if mode == Mode::Both {
                        microphone.len()
                    } else {
                        0
                    });
                    let empty_recording = pending.is_empty()
                        && status
                            .lock()
                            .map(|state| state.recorded_samples == 0)
                            .unwrap_or(false);
                    let result = record_samples(
                        mode,
                        &mut system,
                        &mut microphone,
                        &mut pending,
                        &status,
                        &meeting_id,
                        if tail == 0 && empty_recording {
                            TICK_SAMPLES
                        } else if paused {
                            0
                        } else {
                            tail
                        },
                    );
                    let result = result
                        .and_then(|_| flush(&meeting_id, &mut pending, &status))
                        .and_then(|_| meeting_finish_recording(meeting_id.clone()));
                    let finished = result.is_ok();
                    if let Ok(mut state) = status.lock() {
                        state.active = false;
                        state.paused = !finished;
                        state.pending_save = !finished;
                        state.error = result.as_ref().err().cloned();
                        state.level = 0.0;
                    }
                    paused = !finished;
                    system.clear();
                    microphone.clear();
                    let _ = reply.send(result);
                    if finished {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use cpal::SupportedBufferSize;

        #[test]
        fn failed_stop_keeps_capture_handle_for_a_save_retry() {
            let id = uuid::Uuid::now_v7().to_string();
            let worker_id = id.clone();
            let state = Arc::new(Mutex::new(CaptureStatus {
                meeting_id: id.clone(),
                active: true,
                paused: false,
                level: 0.0,
                recorded_samples: 160,
                error: None,
                pending_save: false,
            }));
            let worker_state = Arc::clone(&state);
            let accepting = Arc::new(AtomicBool::new(true));
            let (sender, receiver) = mpsc::sync_channel(4);
            // Exercise the native stop/handle lifecycle without requesting a
            // physical microphone or writing into a real user's meeting home.
            let worker = thread::spawn(move || {
                let mut attempts = 0;
                while let Ok(Message::Stop(reply)) = receiver.recv() {
                    attempts += 1;
                    if attempts == 1 {
                        worker_state.lock().unwrap().pending_save = true;
                        reply.send(Err("磁盘已满".into())).unwrap();
                    } else {
                        let record: MeetingRecord = serde_json::from_value(serde_json::json!({
                            "id": worker_id, "title": "隔离录音验收", "createdAt": 0, "updatedAt": 0,
                            "status": "recorded", "modelId": "test", "providerId": "test",
                            "durationSeconds": 0.01, "recordedSamples": 160, "audioPath": "unused.wav",
                            "transcript": [], "chunks": []
                        })).unwrap();
                        reply.send(Ok(record)).unwrap();
                        break;
                    }
                }
            });
            *active().lock().unwrap() = Some(ActiveCapture {
                meeting_id: id.clone(),
                sender,
                accepting,
                status: state,
                worker,
            });
            assert!(stop(&id).is_err());
            assert!(status().unwrap().pending_save);
            assert!(active().lock().unwrap().is_some());
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(crate::meeting_capture::finish_before_exit())
                .unwrap();
            assert!(status().is_none());
        }

        #[test]
        fn downmixes_stereo_and_resamples_to_transcription_rate() {
            let config = SupportedStreamConfig::new(
                2,
                48_000,
                SupportedBufferSize::Unknown,
                SampleFormat::F32,
            );
            let mut resampler = Resampler::new(&config);
            let input: Vec<f32> = (0..480).flat_map(|_| [0.5, 0.0]).collect();
            let samples = resampler.consume(&input);
            assert_eq!(samples.len(), 160);
            assert!(samples.iter().all(|sample| (*sample - 0.25).abs() < 0.0001));
        }

        #[test]
        fn upsamples_low_rate_input_without_invalid_samples() {
            let config = SupportedStreamConfig::new(
                1,
                8_000,
                SupportedBufferSize::Unknown,
                SampleFormat::F32,
            );
            let mut resampler = Resampler::new(&config);
            let samples = resampler.consume(&vec![0.5f32; 80]);
            assert_eq!(samples.len(), 160);
            assert!(samples
                .iter()
                .all(|sample| sample.is_finite() && (*sample - 0.5).abs() < 0.0001));
        }
    }
}
