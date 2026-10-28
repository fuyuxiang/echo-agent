---
name: voice-note
description: "Convert voice messages to text (STT) and text to voice (TTS). Supports Whisper local model and Edge-TTS."
version: 1.0.0
metadata:
  echo:
    tags: [Voice, STT, TTS, Whisper, Audio, Media]
---

# Voice Note

Speech-to-Text (STT) and Text-to-Speech (TTS) capabilities.

## Speech → Text

### Option A: OpenAI Whisper API (cloud)

```python
from openai import OpenAI
client = OpenAI()
with open("audio.ogg", "rb") as f:
    transcript = client.audio.transcriptions.create(model="whisper-1", file=f)
print(transcript.text)
```

### Option B: faster-whisper (local, no API key)

```bash
pip install faster-whisper
```

```python
from faster_whisper import WhisperModel

model = WhisperModel("base", compute_type="int8")  # tiny/base/small/medium/large-v3
segments, info = model.transcribe("audio.ogg", language="zh")
text = " ".join(s.text for s in segments)
print(f"[{info.language}] {text}")
```

## Text → Speech

### Edge-TTS (free, excellent Chinese voices)

```bash
pip install edge-tts

# CLI
edge-tts --voice zh-CN-XiaoxiaoNeural --text "你好世界" --write-media output.mp3

# List voices
edge-tts --list-voices | grep zh-CN
```

```python
import edge_tts, asyncio

async def speak(text, voice="zh-CN-XiaoxiaoNeural", output="output.mp3"):
    communicate = edge_tts.Communicate(text, voice)
    await communicate.save(output)

asyncio.run(speak("今天天气不错，适合出门"))
```

### Chinese Voice Options

| Voice | Style |
|-------|-------|
| zh-CN-XiaoxiaoNeural | 女声，活泼自然 |
| zh-CN-YunxiNeural | 男声，温和 |
| zh-CN-YunyangNeural | 男声，新闻播报 |
| zh-CN-XiaoyiNeural | 女声，温柔 |

## Script

```bash
python3 scripts/voice_process.py transcribe audio.ogg --model base --language zh --output transcript.txt
python3 scripts/voice_process.py summarize meeting.mp3 --model small
```

Note: TTS (speak/voices) is in the separate `tts-voice` skill.

## Audio Format Notes

- Input formats: ogg, mp3, wav, m4a, webm (Whisper accepts most)
- Output: mp3 (Edge-TTS default)
- Convert: `ffmpeg -i input.ogg output.mp3`
