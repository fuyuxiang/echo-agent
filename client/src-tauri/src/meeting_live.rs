//! Incremental local recognition reads only durable PCM. Inference never owns
//! the recording write guard.
use super::*;
use std::collections::HashMap;
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
pub(super) struct RequestGuard(String);
impl RequestGuard {
    pub(super) fn acquire(id: &str) -> Result<Self, String> {
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
pub(super) fn phrase_length(samples: &[i16], finalize: bool) -> Option<usize> {
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

pub(super) fn slice_pcm(id: &str, start: u64, count: u64) -> Result<Vec<i16>, String> {
    if count > speech::MAX_WINDOW as u64 {
        return Err("实时音频窗口过大".into());
    }
    let mut file = audio::owned_audio_file(id)?;
    file.seek(SeekFrom::Start(WAV_HEADER_BYTES + start * 2))
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0; count as usize * 2];
    file.read_exact(&mut bytes)
        .map_err(|e| format!("读取已保存录音失败：{e}"))?;
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| i16::from_le_bytes([v[0], v[1]]))
        .collect())
}

pub(super) async fn infer_window(
    meeting_id: &str,
    models: PathBuf,
    cursor: u64,
    end: u64,
) -> Result<speech::Window, String> {
    let start = cursor.saturating_sub(speech::CONTEXT);
    let pcm = slice_pcm(meeting_id, start, end - start)?;
    speech::infer(models, pcm, (cursor - start) as usize).await
}

#[tauri::command]
pub async fn meeting_transcribe_live(
    app: tauri::AppHandle,
    meeting_id: String,
    finalize: bool,
) -> Result<LiveReply, String> {
    transcribe_live(meeting_id, finalize, speech::model_directory(&app)?).await
}

pub(super) async fn transcribe_live(
    meeting_id: String,
    finalize: bool,
    models: PathBuf,
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
    let result = infer_window(&meeting_id, models, cursor, end).await;
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
        Ok(window) => {
            speech::append_window(&mut record, cursor, end, window);
            record.transcript_text = Some(transcript_as_text(&record.transcript));
            record.updated_at = now_millis();
        }
        Err(error) => {
            record.live.error = Some(error.clone());
            save_meeting(&record)?;
            return Err(error);
        }
    }
    save_meeting_content(&record)?;
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
    fn phrases_wait_for_context_and_end_at_silence_or_a_bound() {
        assert_eq!(phrase_length(&vec![1000; 16000], false), None);
        let mut phrase = vec![1000; 16000];
        phrase.extend(vec![0; 8000]);
        assert_eq!(phrase_length(&phrase, false), Some(20800));
        assert_eq!(phrase_length(&vec![1000; 100000], false), Some(64000));
        assert_eq!(phrase_length(&[1000; 80], true), Some(80));
    }
}
