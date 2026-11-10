use super::{MeetingRecord, TranscriptSegment};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tauri::Manager;

#[path = "../meeting-speech/src/protocol.rs"]
mod protocol;
pub(super) use protocol::Window;
pub(super) use protocol::{CONTEXT, MAX_WINDOW};
use protocol::{MAX_REPLY, RATE};

const TIMEOUT: Duration = Duration::from_secs(60);
const MATCH_SIMILARITY: f32 = 0.64;
const MAX_SPEAKER_TRACKS: usize = 64;

static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

struct Worker {
    process: Child,
    input: ChildStdin,
    output: mpsc::Receiver<Vec<u8>>,
}

impl Worker {
    fn start(models: &Path) -> Result<Self, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("无法定位本地转写程序：{error}"))?
            .with_file_name(if cfg!(windows) {
                "echo-meeting-speech.exe"
            } else {
                "echo-meeting-speech"
            });
        #[cfg(test)]
        let executable = std::env::var_os("ECHO_MEETING_TEST_WORKER")
            .map(PathBuf::from)
            .unwrap_or(executable);
        for file in [
            "encoder.int8.onnx",
            "decoder.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
            "pyannote.onnx",
            "eres2net.onnx",
        ] {
            if !models.join(file).is_file() {
                return Err(format!("本地转写模型文件缺失：{file}"));
            }
        }
        let mut command = Command::new(executable);
        command
            .current_dir(models)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut process = command
            .spawn()
            .map_err(|error| format!("无法启动本地转写程序：{error}"))?;
        let input = process.stdin.take().expect("piped stdin");
        let stdout = process.stdout.take().expect("piped stdout");
        let (sender, output) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                if reader
                    .by_ref()
                    .take(MAX_REPLY)
                    .read_until(b'\n', &mut line)
                    .is_err()
                    || line.last() != Some(&b'\n')
                    || sender.send(line).is_err()
                {
                    break;
                }
            }
        });
        let worker = Self {
            process,
            input,
            output,
        };
        if worker.receive(Duration::from_secs(90))? != b"ready\n" {
            return Err("本地转写程序初始化响应无效".into());
        }
        Ok(worker)
    }

    fn receive(&self, timeout: Duration) -> Result<Vec<u8>, String> {
        self.output
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => "本地转写运行超时，可重试".into(),
                mpsc::RecvTimeoutError::Disconnected => "本地转写程序意外退出，可重试".into(),
            })
    }

    fn infer(&mut self, pcm: &[i16], context: usize) -> Result<Window, String> {
        let mut request = Vec::with_capacity(8 + pcm.len() * 2);
        request.extend_from_slice(&(context as u32).to_le_bytes());
        request.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        for sample in pcm {
            request.extend_from_slice(&sample.to_le_bytes());
        }
        self.input
            .write_all(&request)
            .map_err(|error| format!("提交本地转写失败：{error}"))?;
        let line = self.receive(TIMEOUT)?;
        serde_json::from_slice::<Result<Window, String>>(&line).map_err(|_| "本地转写响应无效")?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

pub(super) fn model_directory(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.path()
        .resource_dir()
        .map(|path| path.join("meeting-speech"))
        .map_err(|error| format!("无法定位内置转写资源：{error}"))
}

pub(super) fn preload(models: PathBuf) {
    tauri::async_runtime::spawn_blocking(move || {
        if let Ok(mut worker) = WORKER.try_lock() {
            if worker.is_none() {
                *worker = Worker::start(&models).ok();
            }
        }
    });
}

pub(super) async fn infer(
    models: PathBuf,
    pcm: Vec<i16>,
    context: usize,
) -> Result<Window, String> {
    if pcm.len() > MAX_WINDOW || context >= pcm.len() {
        return Err("本地转写音频范围无效".into());
    }
    if pcm[context..].iter().all(|sample| *sample == 0) {
        return Ok(Window::default());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut worker = WORKER.lock().map_err(|_| "本地转写进程状态不可用")?;
        if worker.is_none() {
            *worker = Some(Worker::start(&models)?);
        }
        let result = worker.as_mut().unwrap().infer(&pcm, context);
        if result.is_err() {
            *worker = None;
        }
        result
    })
    .await
    .map_err(|error| format!("本地转写任务失败：{error}"))?
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Speakers {
    tracks: Vec<Track>,
    recent: Vec<RecentTurn>,
    pending: Vec<usize>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Track {
    label: String,
    centroid: Vec<f32>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct RecentTurn {
    start: f64,
    end: f64,
    track: usize,
}

fn normalized(vector: &[f32]) -> Option<Vec<f32>> {
    if vector.is_empty() || vector.len() > 1024 {
        return None;
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    (norm.is_finite() && norm > 0.0).then(|| vector.iter().map(|value| value / norm).collect())
}

fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

impl Speakers {
    pub(super) fn preserve_names(
        &mut self,
        previous: &[TranscriptSegment],
        edited: &[TranscriptSegment],
        renames: &[(String, String)],
    ) {
        let mut names = HashMap::new();
        for (from, to) in renames.iter().take(MAX_SPEAKER_TRACKS) {
            let to = to.trim();
            if from.is_empty() || to.is_empty() {
                continue;
            }
            let mut group = previous
                .iter()
                .enumerate()
                .filter(|(_, old)| &old.speaker == from);
            if let Some(first) = group.next() {
                if std::iter::once(first).chain(group).all(|(index, old)| {
                    edited.get(index).is_some_and(|new| {
                        old.start == new.start && old.end == new.end && new.speaker == to
                    })
                }) {
                    names.insert(from.as_str(), to);
                }
            }
        }
        for track in &mut self.tracks {
            if let Some(name) = names.get(track.label.as_str()) {
                track.label = (*name).to_owned();
            }
        }
        self.pending = previous
            .iter()
            .zip(edited)
            .filter(|(old, new)| {
                old.start == new.start
                    && old.end == new.end
                    && new.speaker.is_empty()
                    && self.pending.contains(&old.id)
            })
            .map(|(_, new)| new.id)
            .collect();
    }

    fn match_window(
        &mut self,
        window: &Window,
        base: f64,
        cursor: f64,
        occupied: &HashSet<String>,
    ) -> HashMap<i32, usize> {
        let vectors: HashMap<_, _> = window
            .voices
            .iter()
            .filter_map(|voice| Some((voice.speaker, normalized(&voice.embedding)?)))
            .collect();
        let mut local = Vec::new();
        for turn in &window.turns {
            if !local.contains(&turn.speaker) {
                local.push(turn.speaker);
            }
        }
        let mut candidates = Vec::new();
        for &speaker in &local {
            for (track, saved) in self.tracks.iter().enumerate() {
                let voice = vectors
                    .get(&speaker)
                    .map_or(0.0, |vector| similarity(vector, &saved.centroid));
                let overlap = window
                    .turns
                    .iter()
                    .filter(|turn| turn.speaker == speaker)
                    .map(|turn| {
                        self.recent
                            .iter()
                            .filter(|old| old.track == track)
                            .map(|old| {
                                (old.end.min(base + turn.end as f64).min(cursor)
                                    - old.start.max(base + turn.start as f64))
                                .max(0.0)
                            })
                            .sum::<f64>()
                    })
                    .sum::<f64>();
                if voice >= MATCH_SIMILARITY
                    || (overlap >= 0.45 && (voice >= 0.45 || !vectors.contains_key(&speaker)))
                {
                    candidates.push((voice + (overlap.min(2.0) as f32 * 0.18), speaker, track));
                }
            }
        }
        candidates.sort_by(|left, right| right.0.total_cmp(&left.0));
        let mut used = HashSet::new();
        let mut mapping = HashMap::new();
        for (_, speaker, track) in candidates {
            if !mapping.contains_key(&speaker) && used.insert(track) {
                mapping.insert(speaker, track);
            }
        }
        let mut next = self.tracks.len() + 1;
        for speaker in local {
            let Some(voice) = vectors.get(&speaker) else {
                continue;
            };
            if let Some(&track) = mapping.get(&speaker) {
                let saved = &mut self.tracks[track];
                if similarity(&saved.centroid, voice) >= MATCH_SIMILARITY {
                    let blended: Vec<_> = saved
                        .centroid
                        .iter()
                        .zip(voice)
                        .map(|(a, b)| 0.80 * a + 0.20 * b)
                        .collect();
                    if let Some(centroid) = normalized(&blended) {
                        saved.centroid = centroid;
                    }
                }
            } else if self.tracks.len() < MAX_SPEAKER_TRACKS
                && window
                    .turns
                    .iter()
                    .any(|turn| turn.speaker == speaker && base + turn.end as f64 > cursor)
            {
                let label = loop {
                    let candidate = format!("说话人 {next}");
                    next += 1;
                    if !occupied.contains(&candidate)
                        && self.tracks.iter().all(|track| track.label != candidate)
                    {
                        break candidate;
                    }
                };
                mapping.insert(speaker, self.tracks.len());
                self.tracks.push(Track {
                    label,
                    centroid: voice.clone(),
                });
            }
        }
        mapping
    }
}

pub(super) fn append_window(record: &mut MeetingRecord, start: u64, end: u64, window: Window) {
    let cursor = start as f64 / RATE as f64;
    let stop = end as f64 / RATE as f64;
    let base = start.saturating_sub(CONTEXT) as f64 / RATE as f64;
    let occupied = record
        .transcript
        .iter()
        .map(|segment| segment.speaker.clone())
        .collect();
    let speakers = &mut record.speakers;
    let mapping = speakers.match_window(&window, base, cursor, &occupied);
    let turns: Vec<_> = window
        .turns
        .iter()
        .filter_map(|turn| {
            Some(RecentTurn {
                start: base + turn.start as f64,
                end: (base + turn.end as f64).min(stop),
                track: *mapping.get(&turn.speaker)?,
            })
        })
        .collect();

    // A later overlapping window can resolve an initially uncertain label.
    // Only passages created by the local engine and still unlabelled qualify.
    let mut resolved = HashSet::new();
    for segment in record
        .transcript
        .iter_mut()
        .rev()
        .take_while(|segment| segment.end >= base)
    {
        if segment.speaker.is_empty() && speakers.pending.contains(&segment.id) {
            if let Some(turn) = turns
                .iter()
                .filter(|turn| turn.start < segment.end && turn.end > segment.start)
                .max_by(|left, right| {
                    let overlap = |turn: &RecentTurn| {
                        turn.end.min(segment.end) - turn.start.max(segment.start)
                    };
                    overlap(left).total_cmp(&overlap(right))
                })
            {
                segment
                    .speaker
                    .clone_from(&speakers.tracks[turn.track].label);
                resolved.insert(segment.id);
            }
        }
    }
    speakers.pending.retain(|id| {
        !resolved.contains(id)
            && record
                .transcript
                .iter()
                .rev()
                .take_while(|segment| segment.end >= base)
                .any(|segment| segment.id == *id)
    });
    speakers.recent = turns;

    let first_new_id = record
        .transcript
        .iter()
        .map(|segment| segment.id)
        .max()
        .map_or(0, |id| id.saturating_add(1));
    let mut next_id = first_new_id;
    for piece in window.pieces {
        let start = (cursor + piece.start as f64).clamp(cursor, stop);
        let end = (cursor + piece.end as f64).clamp(start, stop);
        if end <= start || piece.text.trim().is_empty() {
            continue;
        }
        // Legacy partial records may already contain saved/corrected text
        // later than their cursor. Do not replace or duplicate those passages.
        if record
            .transcript
            .iter()
            .rev()
            .take_while(|segment| segment.end > cursor)
            .any(|segment| segment.id < first_new_id && segment.start < end && segment.end > start)
        {
            continue;
        }
        let speaker = piece
            .speaker
            .and_then(|speaker| mapping.get(&speaker))
            .map(|track| speakers.tracks[*track].label.clone())
            .unwrap_or_default();
        if speaker.is_empty() {
            speakers.pending.push(next_id);
        }
        record.transcript.push(TranscriptSegment {
            id: next_id,
            start,
            end,
            speaker,
            text: piece.text,
        });
        next_id = next_id.saturating_add(1);
    }
    record
        .transcript
        .sort_by(|left, right| left.start.total_cmp(&right.start));
    record.live.processed_samples = end;
    record.live.error = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{Piece, Turn, Voice};

    fn window(local: i32, vector: Vec<f32>, time: f32, text: &str) -> Window {
        Window {
            pieces: vec![Piece {
                start: 0.0,
                end: 1.0,
                speaker: Some(local),
                text: text.into(),
            }],
            turns: vec![Turn {
                start: time,
                end: time + 1.0,
                speaker: local,
            }],
            voices: vec![Voice {
                speaker: local,
                embedding: vector,
            }],
        }
    }

    #[test]
    fn speakers_keep_identity_across_windows_and_saved_renames() {
        let mut record = super::super::tests::test_record(PathBuf::new());
        let samples = u64::from(RATE);
        append_window(
            &mut record,
            0,
            samples,
            window(4, vec![1.0, 0.0], 0.0, "第一位"),
        );
        append_window(
            &mut record,
            samples,
            samples * 2,
            window(4, vec![0.0, 1.0], 1.0, "第二位"),
        );
        let mut edited = record.transcript.clone();
        edited[0].speaker = "陈晨".into();
        edited[0].text = "人工校对".into();
        record
            .speakers
            .preserve_names(&record.transcript, &edited, &[]);
        assert_eq!(record.speakers.tracks[0].label, "说话人 1");
        record.speakers.preserve_names(
            &record.transcript,
            &edited,
            &[("说话人 1".into(), "陈晨".into())],
        );
        record.transcript = edited;
        let saved = serde_json::to_string(&record.speakers).unwrap();
        record.speakers = serde_json::from_str(&saved).unwrap();
        append_window(
            &mut record,
            12 * samples,
            13 * samples,
            window(7, vec![0.95, 0.05], 8.0, "第一位再次发言"),
        );
        assert_eq!(
            record
                .transcript
                .iter()
                .map(|piece| piece.speaker.as_str())
                .collect::<Vec<_>>(),
            ["陈晨", "说话人 2", "陈晨"]
        );
        assert_eq!(record.transcript[0].text, "人工校对");
        assert_eq!(record.transcript[2].start, 12.0);
        assert_eq!(record.live.processed_samples, 13 * samples);
        assert!(serde_json::to_value(&record)
            .unwrap()
            .get("localSpeakers")
            .is_none());
    }

    #[test]
    fn unmatched_speakers_cannot_grow_the_persisted_state_without_bound() {
        let mut record = super::super::tests::test_record(PathBuf::new());
        let samples = u64::from(RATE);
        for index in 0..=MAX_SPEAKER_TRACKS {
            let mut voice = vec![0.0; MAX_SPEAKER_TRACKS + 1];
            voice[index] = 1.0;
            let start = index as u64 * 10 * samples;
            let time = start.min(CONTEXT) as f32 / RATE as f32;
            append_window(
                &mut record,
                start,
                start + samples,
                window(0, voice, time, "原文仍可识别"),
            );
        }
        assert_eq!(record.speakers.tracks.len(), MAX_SPEAKER_TRACKS);
        assert!(record.transcript.last().unwrap().speaker.is_empty());
    }

    #[test]
    fn a_later_window_resolves_a_short_turn_without_rewriting_the_text() {
        let mut record = super::super::tests::test_record(PathBuf::new());
        let samples = u64::from(RATE);
        let mut short = window(2, vec![], 0.0, "短句");
        short.voices.clear();
        append_window(&mut record, 0, samples, short);
        assert_eq!(record.transcript[0].speaker, "");
        let mut followup = window(8, vec![1.0, 0.0], 0.0, "继续说话");
        followup.turns[0].end = 2.0;
        append_window(&mut record, samples, 2 * samples, followup);
        assert_eq!(record.transcript[0].speaker, record.transcript[1].speaker);
        assert_eq!(record.transcript[0].speaker, "说话人 1");
        assert_eq!(record.transcript[0].text, "短句");
    }
}
