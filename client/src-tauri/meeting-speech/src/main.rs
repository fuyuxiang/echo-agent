mod protocol;

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use protocol::{Piece, Turn, Voice, Window, CONTEXT, MAX_REPLY, MAX_WINDOW, RATE};
use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSpeakerDiarization,
    OfflineSpeakerDiarizationConfig, OfflineTransducerModelConfig, SpeakerEmbeddingExtractor,
    SpeakerEmbeddingExtractorConfig,
};

struct Engine {
    recognizer: OfflineRecognizer,
    diarizer: OfflineSpeakerDiarization,
    embedder: SpeakerEmbeddingExtractor,
}

impl Engine {
    fn load() -> Result<Self, String> {
        // The parent sets the working directory to the bundle resource directory.
        // Relative ASCII model names also work in non-ASCII Windows install paths.
        let mut asr = OfflineRecognizerConfig::default();
        asr.model_config.transducer = OfflineTransducerModelConfig {
            encoder: Some("encoder.int8.onnx".into()),
            decoder: Some("decoder.onnx".into()),
            joiner: Some("joiner.int8.onnx".into()),
        };
        asr.model_config.tokens = Some("tokens.txt".into());
        asr.model_config.num_threads = 2;
        let embedding = SpeakerEmbeddingExtractorConfig {
            model: Some("eres2net.onnx".into()),
            num_threads: 2,
            ..Default::default()
        };
        let mut diarization = OfflineSpeakerDiarizationConfig {
            embedding: embedding.clone(),
            min_duration_on: 0.20,
            min_duration_off: 0.18,
            ..Default::default()
        };
        diarization.segmentation.pyannote.model = Some("pyannote.onnx".into());
        diarization.segmentation.num_threads = 2;
        diarization.clustering.threshold = 0.82;
        Ok(Self {
            recognizer: OfflineRecognizer::create(&asr).ok_or("X-ASR 初始化失败")?,
            diarizer: OfflineSpeakerDiarization::create(&diarization)
                .ok_or("说话人分段模型初始化失败")?,
            embedder: SpeakerEmbeddingExtractor::create(&embedding)
                .ok_or("说话人声纹模型初始化失败")?,
        })
    }

    fn infer(&self, samples: &[f32], context: usize) -> Result<Window, String> {
        let target = &samples[context..];
        if target.iter().all(|sample| *sample == 0.0) {
            return Ok(Window::default());
        }
        let offset = context as f32 / RATE as f32;
        let end = samples.len() as f32 / RATE as f32;
        let mut padded = samples.to_vec();
        padded.resize(samples.len().max(RATE as usize * 10), 0.0);
        let turns: Vec<_> = self
            .diarizer
            .process(&padded)
            .ok_or("本地说话人分段失败")?
            .sort_by_start_time()
            .into_iter()
            .filter(|turn| turn.start.is_finite() && turn.end.is_finite() && turn.speaker >= 0)
            .filter_map(|turn| {
                let start = turn.start.max(0.0);
                let stop = turn.end.min(end);
                (stop > start).then_some(Turn {
                    start,
                    end: stop,
                    speaker: turn.speaker,
                })
            })
            .collect();

        // Only use exclusive speech for speaker embeddings. Feeding two voices
        // together biases both tracks toward the same centroid.
        let mut speech = BTreeMap::<i32, Vec<f32>>::new();
        for turn in &turns {
            let waveform = speech.entry(turn.speaker).or_default();
            let start = (turn.start * RATE as f32).round() as usize;
            let end = ((turn.end * RATE as f32).round() as usize).min(samples.len());
            let frame = RATE as usize / 50;
            for (index, chunk) in samples[start.min(end)..end].chunks(frame).enumerate() {
                if waveform.len() >= RATE as usize * 3 {
                    break;
                }
                let at = (start + index * frame + chunk.len() / 2) as f32 / RATE as f32;
                if !turns.iter().any(|other| {
                    other.speaker != turn.speaker && other.start <= at && at < other.end
                }) {
                    waveform.extend_from_slice(chunk);
                }
            }
        }
        let voices = speech
            .into_iter()
            .filter_map(|(speaker, wave)| {
                if wave.len() < RATE as usize / 2 {
                    return None;
                }
                let stream = self.embedder.create_stream()?;
                stream.accept_waveform(RATE as i32, &wave);
                stream.input_finished();
                if !self.embedder.is_ready(&stream) {
                    return None;
                }
                let embedding = self.embedder.compute(&stream)?;
                embedding
                    .iter()
                    .all(|value| value.is_finite())
                    .then_some(Voice { speaker, embedding })
            })
            .collect();

        let stream = self.recognizer.create_stream();
        let mut target_padded = target.to_vec();
        target_padded.resize(target.len().max(RATE as usize / 2), 0.0);
        stream.accept_waveform(RATE as i32, &target_padded);
        self.recognizer.decode(&stream);
        let result = stream.get_result().ok_or("本地语音识别失败")?;
        let duration = target.len() as f32 / RATE as f32;
        let pieces = split_text(result, &turns, offset, duration);
        Ok(Window {
            pieces,
            turns,
            voices,
        })
    }
}

fn speaker_at(turns: &[Turn], start: f32, end: f32) -> Option<i32> {
    turns
        .iter()
        .map(|turn| (turn.speaker, turn.end.min(end) - turn.start.max(start)))
        .filter(|(_, overlap)| *overlap > 0.0)
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(speaker, _)| speaker)
}

fn clean_spacing(text: &str) -> String {
    let han = |ch| matches!(ch, '\u{3400}'..='\u{9fff}' | '\u{20000}'..='\u{323af}');
    let mut result = String::new();
    let mut previous = None;
    let mut space = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            space = true;
            continue;
        }
        if space
            && previous.is_some_and(|last| {
                !(han(last) && han(ch))
                    && !"，。！？、；：,.!?;:）】".contains(ch)
                    && !"（【".contains(last)
                    && !(han(ch) && "，。！？、；：".contains(last))
            })
        {
            result.push(' ');
        }
        result.push(ch);
        previous = Some(ch);
        space = false;
    }
    result
}

fn split_text(
    result: sherpa_onnx::OfflineRecognizerResult,
    turns: &[Turn],
    offset: f32,
    duration: f32,
) -> Vec<Piece> {
    if result.text.trim().is_empty() {
        return Vec::new();
    }
    let fallback = || Piece {
        start: 0.0,
        end: duration,
        speaker: speaker_at(turns, offset, offset + duration),
        text: clean_spacing(&result.text),
    };
    let Some(times) = result.timestamps.as_ref().filter(|times| {
        times.len() == result.tokens.len()
            && !times.is_empty()
            && times.iter().all(|time| time.is_finite())
    }) else {
        return vec![fallback()];
    };

    // Align tokens to speaker turns before forming passages so a speaker
    // change inside a sentence does not give both voices the same label.
    let mut groups: Vec<(Piece, Vec<u8>)> = Vec::new();
    for (index, token) in result.tokens.iter().enumerate() {
        let content = token.trim_matches(|ch: char| ch == '▁' || ch.is_whitespace());
        if content.is_empty() {
            continue;
        }
        let trailing_punctuation = content
            .chars()
            .all(|ch| "，。！？、；：,.!?;:".contains(ch));
        if trailing_punctuation {
            if let Some((_, text)) = groups.last_mut() {
                text.extend_from_slice(content.as_bytes());
            }
            continue;
        }
        let start = times[index].clamp(0.0, duration);
        let end = times
            .get(index + 1)
            .copied()
            .unwrap_or(start + 0.22)
            .clamp(start, (start + 0.30).min(duration))
            .max((start + 0.06).min(duration));
        let mut speaker = speaker_at(turns, offset + start, offset + end);
        if let Some((previous, _)) = groups.last() {
            if speaker.is_none() && start <= previous.end + 0.18 {
                speaker = previous.speaker;
            }
        }
        // Byte-fallback tokens can represent fragments of one Unicode codepoint.
        let byte = token
            .strip_prefix("<0x")
            .and_then(|text| text.strip_suffix('>'))
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        let same_group = groups.last().is_some_and(|(previous, bytes)| {
            (byte.is_some() && std::str::from_utf8(bytes).is_err())
                || (previous.speaker == speaker && start <= previous.end + 0.65)
        });
        if !same_group {
            groups.push((
                Piece {
                    start,
                    end,
                    speaker,
                    text: String::new(),
                },
                Vec::new(),
            ));
        }
        let (piece, text) = groups.last_mut().unwrap();
        piece.end = piece.end.max(end);
        if let Some(byte) = byte {
            text.push(byte);
        } else {
            text.extend_from_slice(token.replace('▁', " ").as_bytes());
        }
    }
    if groups.len() == 1 {
        let (mut piece, _) = groups.pop().unwrap();
        piece.text = clean_spacing(&result.text);
        if piece.end <= piece.start {
            piece.start = 0.0;
            piece.end = duration;
        }
        return vec![piece];
    }
    groups
        .into_iter()
        .filter_map(|(mut piece, text)| {
            piece.text = clean_spacing(&String::from_utf8_lossy(&text));
            if !piece.text.is_empty() && piece.end <= piece.start {
                piece.start = (piece.end - 0.06).max(0.0);
            }
            (!piece.text.is_empty() && piece.end > piece.start).then_some(piece)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_chinese_spacing_and_keeps_english_word_boundaries() {
        assert_eq!(
            clean_spacing("  这 是 一 句 ， 下 一 句。  "),
            "这是一句，下一句。"
        );
        assert_eq!(
            clean_spacing("用 Echo Agent 开 会 。"),
            "用 Echo Agent 开会。"
        );
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = io::stdin().lock();
    let mut output = io::BufWriter::new(io::stdout().lock());
    let engine = Engine::load()?;
    output.write_all(b"ready\n")?;
    output.flush()?;
    loop {
        let mut header = [0u8; 8];
        match input.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let context = u32::from_le_bytes(header[..4].try_into()?) as usize;
        let count = u32::from_le_bytes(header[4..].try_into()?) as usize;
        if count > MAX_WINDOW || context >= count || context as u64 > CONTEXT {
            return Err("语音推理请求范围无效".into());
        }
        let mut bytes = vec![0u8; count * 2];
        input.read_exact(&mut bytes)?;
        let pcm: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32_768.0)
            .collect();
        let reply = serde_json::to_vec(&engine.infer(&pcm, context))?;
        if reply.len() as u64 >= MAX_REPLY {
            return Err("语音推理响应超过限制".into());
        }
        output.write_all(&reply)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
