//! Incremental short-phrase recognition using the existing MiniMax file API.
//! Network work never owns the recording/write guard. Only durable PCM is sent.
use super::*;
use std::sync::{Arc, Weak};
static WRITES: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
pub(super) fn write_lock(id: &str) -> Result<Arc<Mutex<()>>, String> {
    let mut locks = WRITES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "录音保存状态不可用")?;
    locks.retain(|_, value| value.strong_count() > 0);
    if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(id.into(), Arc::downgrade(&lock));
    Ok(lock)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveProgress {
    #[serde(default)]
    pub processed_samples: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveReply {
    pub record: MeetingRecord,
    pub waiting: bool,
}

static REQUESTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
struct RequestGuard(String);
impl RequestGuard {
    fn acquire(id: &str) -> Result<Self, String> {
        let mut requests = REQUESTS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "转写状态不可用")?;
        if !requests.insert(id.into()) {
            return Err("该录音已有短句正在转写".into());
        }
        Ok(Self(id.into()))
    }
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if let Ok(mut requests) = REQUESTS.get_or_init(Default::default).lock() {
            requests.remove(&self.0);
        }
    }
}

// End a phrase at a quiet boundary. Continuous speech is bounded to four seconds;
// finalized recordings drain larger windows without losing the short final tail.
fn phrase_length(samples: &[i16], finalize: bool) -> Option<usize> {
    let minimum = SAMPLE_RATE as usize * 6 / 5;
    let maximum = SAMPLE_RATE as usize * if finalize { 8 } else { 4 };
    let limit = samples.len().min(maximum);
    if finalize {
        return (limit > 0).then_some(limit);
    }
    if limit < minimum {
        return None;
    }
    let frame = SAMPLE_RATE as usize / 50;
    let mut quiet = 0usize;
    for (index, values) in samples[..limit].chunks(frame).enumerate() {
        let rms =
            (values.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / values.len() as f64).sqrt();
        quiet = if rms < 180.0 { quiet + values.len() } else { 0 };
        let end = ((index + 1) * frame).min(limit);
        if end >= minimum && quiet >= SAMPLE_RATE as usize * 3 / 10 {
            return Some(end);
        }
    }
    (limit == maximum).then_some(limit)
}

fn slice_pcm(id: &str, start: u64, count: u64) -> Result<Vec<i16>, String> {
    if count > SAMPLE_RATE as u64 * 9 {
        return Err("实时音频窗口过大".into());
    }
    let mut file = audio::owned_audio_file(id)?;
    file.seek(SeekFrom::Start(WAV_HEADER_BYTES + start * 2))
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0; count as usize * 2];
    file.read_exact(&mut bytes)
        .map_err(|e| format!("读取已保存录音失败：{e}"))?;
    Ok(bytes
        .chunks_exact(2)
        .map(|v| i16::from_le_bytes([v[0], v[1]]))
        .collect())
}

fn append_result(record: &mut MeetingRecord, start: u64, end: u64, response: AsrResponse) {
    let chunk = MeetingChunk {
        index: 0,
        start: start as f64 / SAMPLE_RATE as f64,
        end: end as f64 / SAMPLE_RATE as f64,
        status: "completed".into(),
        trace_id: None,
        error: None,
    };
    let boundary = record.live.processed_samples as f64 / SAMPLE_RATE as f64;
    let mut incoming = normalize_transcript(&[(chunk, response)], &[]);
    incoming.retain(|s| s.start.is_finite() && s.end.is_finite() && s.end > boundary + 0.01);
    for mut segment in incoming {
        segment.start = segment
            .start
            .max(boundary)
            .min(end as f64 / SAMPLE_RATE as f64);
        segment.end = segment
            .end
            .min(end as f64 / SAMPLE_RATE as f64)
            .max(segment.start);
        if start < record.live.processed_samples {
            if let Some(last) = record.transcript.last() {
                // Remove a literal repeated boundary, retaining repeated phrases
                // elsewhere. The context overlap is only half a second.
                let before: Vec<char> = last.text.chars().collect();
                let after: Vec<char> = segment.text.chars().collect();
                for length in (3..=before.len().min(after.len()).min(8)).rev() {
                    if before[before.len() - length..] == after[..length] {
                        segment.text = after[length..]
                            .iter()
                            .collect::<String>()
                            .trim()
                            .to_string();
                        break;
                    }
                }
            }
        }
        if segment.text.trim().is_empty() || segment.end <= segment.start {
            continue;
        }
        segment.id = record.transcript.len();
        record.transcript.push(segment);
    }
    record.transcript_text = Some(transcript_as_text(&record.transcript));
    record.live.processed_samples = end;
    record.live.error = None;
    record.updated_at = now_millis();
}

/// Repair a recording's connection after settings change, without interrupting
/// capture or dropping its durable transcript/cursor.
#[tauri::command]
pub fn meeting_set_transcription_connection(
    meeting_id: String,
    model_id: String,
    provider_id: String,
) -> Result<MeetingRecord, String> {
    let lock = write_lock(&meeting_id)?;
    let _lock = lock.lock().map_err(|_| "录音保存状态不可用")?;
    let _job = MeetingJobGuard::acquire(&meeting_id)?;
    let mut record = read_meeting(&meeting_id)?;
    if !matches!(record.status.as_str(), "recording" | "paused") {
        return Err("当前录音已结束，请在录音详情中继续处理".into());
    }
    processing_provider(&mut record, Some(model_id), Some(provider_id))?;
    record.live.error = None;
    record.updated_at = now_millis();
    save_meeting(&record)?;
    Ok(record)
}

#[tauri::command]
pub async fn meeting_transcribe_live(
    meeting_id: String,
    finalize: bool,
) -> Result<LiveReply, String> {
    validate_id(&meeting_id)?;
    let _request = RequestGuard::acquire(&meeting_id)?;
    let snapshot = read_meeting(&meeting_id)?;
    if !matches!(
        snapshot.status.as_str(),
        "recording" | "paused" | "recorded" | "interrupted" | "partial" | "failed"
    ) {
        return Ok(LiveReply {
            record: snapshot,
            waiting: true,
        });
    }
    let cursor = snapshot
        .live
        .processed_samples
        .min(snapshot.recorded_samples);
    let available = (snapshot.recorded_samples - cursor).min(SAMPLE_RATE as u64 * 8);
    if available == 0 {
        return Ok(LiveReply {
            record: snapshot,
            waiting: true,
        });
    }
    let samples = slice_pcm(&meeting_id, cursor, available)?;
    let Some(length) = phrase_length(&samples, finalize || snapshot.status == "paused") else {
        return Ok(LiveReply {
            record: snapshot,
            waiting: true,
        });
    };
    let end = cursor + length as u64;
    let start = cursor.saturating_sub(SAMPLE_RATE as u64 / 2);
    let result = async {
        // Digital silence is accounted for locally; no fabricated transcript.
        if samples[..length].iter().all(|sample| *sample == 0) {
            return Ok(AsrResponse {
                text: String::new(),
                segments: vec![],
                trace_id: None,
            });
        }
        let access =
            crate::providers::meeting_provider_access(&snapshot.model_id, &snapshot.provider_id)?;
        let pcm = slice_pcm(&meeting_id, start, end - start)?;
        let mut wav = Vec::with_capacity(44 + pcm.len() * 2);
        write_wav_header(&mut wav, pcm.len() as u32 * 2).map_err(|e| e.to_string())?;
        for sample in pcm {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(25))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        transcribe_bytes(
            &client,
            &asr_endpoint(&access.base_url),
            &access.api_key,
            wav,
        )
        .await
    }
    .await;
    // PCM may have advanced while ASR was running. Reload, then change only the
    // transcript/cursor; never overwrite counters, pause state or audio paths.
    let lock = write_lock(&meeting_id)?;
    let _lock = lock.lock().map_err(|_| "录音保存状态不可用")?;
    let _write = MeetingJobGuard::acquire(&meeting_id)?;
    let mut record = read_meeting(&meeting_id)?;
    if record.live.processed_samples != cursor
        || !matches!(
            record.status.as_str(),
            "recording" | "paused" | "recorded" | "interrupted" | "partial" | "failed"
        )
    {
        return Ok(LiveReply {
            record,
            waiting: true,
        });
    }
    match result {
        Ok(response) => append_result(&mut record, start, end, response),
        Err(error) => {
            record.live.error = Some(error.clone());
            save_meeting(&record)?;
            return Err(error);
        }
    }
    save_meeting(&record)?;
    Ok(LiveReply {
        record,
        waiting: false,
    })
}

#[tauri::command]
pub fn meeting_restore_minutes(meeting_id: String) -> Result<MeetingRecord, String> {
    let _write = MeetingJobGuard::acquire(&meeting_id)?;
    let mut record = read_meeting(&meeting_id)?;
    let previous = record.previous_minutes.take().ok_or("没有可恢复的纪要")?;
    record.previous_minutes = record.minutes.replace(previous);
    record.minutes_stale = true;
    record.updated_at = now_millis();
    save_meeting(&record)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_transport_and_recording_recovery_are_isolated() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "meeting_minutes::live::tests::live_transport_child",
                "--nocapture",
            ])
            .env("ECHO_AGENT_HOME", home.path())
            .env("ECHO_MEETING_LIVE_CHILD", "1")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn live_transport_child() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if std::env::var("ECHO_MEETING_LIVE_CHILD").as_deref() != Ok("1") {
            return;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for index in 0..5 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0u8; 8192];
                    let count = stream.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(offset) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..offset]).to_ascii_lowercase();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap();
                        if bytes.len() >= offset + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&bytes).to_string();
                let (status, body) = if index == 1 {
                    (503, r#"{"error":{"message":"temporary offline"}}"#)
                } else if index == 2 {
                    (
                        200,
                        r#"{"base_resp":{"status_code":1002,"status_msg":"quota unavailable"}}"#,
                    )
                } else if index < 4 {
                    (
                        200,
                        r#"{"text":"确认产品方案并开始测试","segments":[{"start":0,"end":4.5,"text":"确认产品方案并开始测试"}]}"#,
                    )
                } else {
                    (
                        200,
                        r###"{"choices":[{"message":{"content":"## 会议摘要\n确认产品方案并开始测试。"}}]}"###,
                    )
                };
                if index == 0 {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                let response = format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await.unwrap();
                requests.push(request);
            }
            requests
        });
        let config = format!(
            r#"
            [model_providers.test]
            base_url = "http://{address}/v1"
            api_backend = "chat_completions"
            auth_scheme = "bearer"
            api_key = "isolated-live-test-key"
            echoagent_allow_insecure_http = true
            [model."test/MiniMax"]
            model_provider = "test"
            model = "MiniMax"
        "#
        );
        crate::paths::write_private_file(
            &crate::paths::echo_agent_home_dir().join("config.toml"),
            config.as_bytes(),
        )
        .unwrap();
        let record = meeting_create(
            "增量验收".into(),
            "test/MiniMax".into(),
            "test".into(),
            Some("microphone".into()),
        )
        .unwrap();
        meeting_append_pcm(record.id.clone(), vec![1500; 64000]).unwrap();
        let live_id = record.id.clone();
        let append_id = record.id.clone();
        let (first, _) = tokio::join!(meeting_transcribe_live(live_id, false), async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            meeting_append_pcm(append_id, vec![1500; 8000]).unwrap();
        });
        let first = first.unwrap();
        assert_eq!(
            first.record.recorded_samples, 72000,
            "ASR must not roll back concurrent PCM writes"
        );
        assert_eq!(first.record.live.processed_samples, 64000);
        assert_eq!(first.record.transcript.len(), 1);
        let before_repair = read_meeting(&record.id).unwrap();
        assert!(meeting_set_transcription_connection(
            record.id.clone(),
            "missing-model".into(),
            "missing-provider".into()
        )
        .is_err());
        assert_eq!(
            read_meeting(&record.id).unwrap().model_id,
            before_repair.model_id
        );
        let rebound = meeting_set_transcription_connection(
            record.id.clone(),
            "test/MiniMax".into(),
            "test".into(),
        )
        .unwrap();
        assert_eq!(rebound.transcript, before_repair.transcript);
        assert_eq!(
            rebound.live.processed_samples,
            before_repair.live.processed_samples
        );
        assert_eq!(rebound.recorded_samples, before_repair.recorded_samples);
        assert_eq!(rebound.status, "recording");
        meeting_append_pcm(record.id.clone(), vec![1500; 56000]).unwrap();
        assert!(meeting_transcribe_live(record.id.clone(), false)
            .await
            .is_err());
        let failed = read_meeting(&record.id).unwrap();
        assert_eq!(failed.status, "recording");
        assert_eq!(failed.recorded_samples, 128000);
        assert_eq!(failed.live.processed_samples, 64000);
        assert!(failed.live.error.is_some());
        assert!(meeting_transcribe_live(record.id.clone(), false)
            .await
            .unwrap_err()
            .contains("未完成识别"));
        assert_eq!(
            read_meeting(&record.id).unwrap().live.processed_samples,
            64000
        );
        let retried = meeting_transcribe_live(record.id.clone(), false)
            .await
            .unwrap();
        assert_eq!(retried.record.live.processed_samples, 128000);
        assert!(retried.record.live.error.is_none());
        meeting_finish_recording(record.id.clone()).unwrap();
        assert!(meeting_set_transcription_connection(
            record.id.clone(),
            "test/MiniMax".into(),
            "test".into()
        )
        .is_err());
        meeting_update(
            record.id.clone(),
            None,
            None,
            Some("人工旧纪要".into()),
            None,
        )
        .unwrap();
        let completed = meeting_process(record.id.clone(), None, None)
            .await
            .unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.previous_minutes.as_deref(), Some("人工旧纪要"));
        assert!(
            completed.chunks.is_empty(),
            "completed live audio must not be transcribed twice"
        );
        let token = audio::meeting_audio_token(record.id.clone()).unwrap();
        let response = audio::respond(
            tauri::http::Request::builder()
                .uri(format!("meeting-audio://localhost/{}/{token}", record.id))
                .header("Range", "bytes=44-47")
                .body(vec![])
                .unwrap(),
            true,
        );
        assert_eq!(response.status(), 206);
        assert_eq!(response.body(), &vec![220, 5, 220, 5]);
        let encoded = audio::respond(
            tauri::http::Request::builder()
                .uri(format!(
                    "http://meeting-audio.localhost/{}%2F{token}",
                    record.id
                ))
                .header("Range", "bytes=44-47")
                .body(vec![])
                .unwrap(),
            true,
        );
        assert_eq!(encoded.status(), 206);
        assert_eq!(encoded.body(), response.body());
        let total = SAMPLE_RATE as u64 * 7200 * 2 + WAV_HEADER_BYTES;
        let path = meeting_dir(&record.id).unwrap().join("recording.wav");
        let mut sparse = OpenOptions::new().write(true).open(path).unwrap();
        write_wav_header(&mut sparse, (total - WAV_HEADER_BYTES) as u32).unwrap();
        sparse.set_len(total).unwrap();
        drop(sparse);
        let end = audio::respond(
            tauri::http::Request::builder()
                .uri(format!("meeting-audio://localhost/{}/{token}", record.id))
                .header("Range", format!("bytes={}-", total - 32))
                .body(vec![])
                .unwrap(),
            true,
        );
        assert_eq!(end.status(), 206);
        assert_eq!(end.body().len(), 32);
        assert_eq!(
            end.headers()["Content-Range"],
            format!("bytes {}-{}/{total}", total - 32, total - 1)
        );
        let requests = server.await.unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|v| v.starts_with("POST /v1/speech_to_text "))
                .count(),
            4
        );
        assert!(
            requests[0].contains("RIFF")
                && requests[0].contains("asr-1.0")
                && requests[0].contains("verbose_json")
        );
        assert_eq!(
            requests
                .iter()
                .filter(|v| v.starts_with("POST /v1/chat/completions "))
                .count(),
            1
        );
    }
    #[test]
    fn phrases_wait_for_context_and_end_at_silence_or_a_bound() {
        assert_eq!(phrase_length(&vec![1000; 16000], false), None);
        let mut phrase = vec![1000; 16000];
        phrase.extend(vec![0; 8000]);
        assert_eq!(phrase_length(&phrase, false), Some(20800));
        assert_eq!(phrase_length(&vec![1000; 100000], false), Some(64000));
        assert_eq!(phrase_length(&[1000; 80], true), Some(80));
    }
    #[test]
    fn committed_results_keep_audio_counters_and_remove_only_boundary_duplicates() {
        let mut record = super::super::tests::test_record(PathBuf::from("unused"));
        record.recorded_samples = 160000;
        record.status = "paused".into();
        record.live.processed_samples = 64000;
        record.transcript.push(TranscriptSegment {
            id: 0,
            start: 0.,
            end: 4.,
            speaker: "S1".into(),
            text: "本周完成产品设计".into(),
        });
        append_result(
            &mut record,
            56000,
            128000,
            AsrResponse {
                text: "产品设计并确认测试范围".into(),
                segments: vec![],
                trace_id: None,
            },
        );
        assert_eq!(record.recorded_samples, 160000);
        assert_eq!(record.status, "paused");
        assert_eq!(record.live.processed_samples, 128000);
        assert_eq!(record.transcript[1].text, "并确认测试范围");
        assert_eq!(record.transcript[1].start, 4.);
    }
}
