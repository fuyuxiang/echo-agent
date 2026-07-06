---
name: tts-voice
description: "Convert text to natural speech audio. Uses Edge-TTS (free) or OpenAI TTS. Excellent Chinese voice support."
version: 1.0.0
metadata:
  echo:
    tags: [TTS, Voice, Audio, Speech, Media]
---

# TTS Voice

Text-to-Speech with natural Chinese voices.

## Edge-TTS (Free, Recommended)

```bash
pip install edge-tts
```

### CLI

```bash
edge-tts --voice zh-CN-XiaoxiaoNeural --text "今天天气不错" --write-media /tmp/output.mp3
edge-tts --list-voices | grep zh-CN
```

### Python

```python
import edge_tts, asyncio

async def speak(text, voice="zh-CN-XiaoxiaoNeural", output="output.mp3"):
    communicate = edge_tts.Communicate(text, voice)
    await communicate.save(output)

asyncio.run(speak("欢迎使用 Echo Agent"))
```

## Chinese Voices

| Voice ID | Style |
|----------|-------|
| zh-CN-XiaoxiaoNeural | 女声，活泼自然 |
| zh-CN-YunxiNeural | 男声，温和 |
| zh-CN-YunyangNeural | 男声，新闻播报 |
| zh-CN-XiaoyiNeural | 女声，温柔 |
| zh-CN-liaoning-XiaobeiNeural | 东北方言 |
| zh-TW-HsiaoChenNeural | 台湾女声 |

## OpenAI TTS (Alternative)

Requires `OPENAI_API_KEY`:

```python
from openai import OpenAI
client = OpenAI()
response = client.audio.speech.create(
    model="tts-1",  # or tts-1-hd
    voice="alloy",  # alloy/echo/fable/onyx/nova/shimmer
    input="Hello world"
)
response.stream_to_file("output.mp3")
```

## Script

```bash
python3 scripts/text_to_speech.py "你好世界"
python3 scripts/text_to_speech.py "长文本内容..." --voice zh-CN-YunxiNeural -o briefing.mp3
python3 scripts/text_to_speech.py --list-voices zh
```

## Delivering the audio (important for scheduled/unattended tasks)

Generating an mp3 does NOT send it. When the user should actually receive the
audio (e.g. a cron-triggered morning briefing), use the built-in
`text_to_speech` tool with `deliver=true` so synthesis and delivery happen in
one step:

```
text_to_speech(text="北京今天多云…", voice="zh-CN-XiaoxiaoNeural", deliver=true)
```

With `deliver=true` the tool sends the file to the current chat automatically
(target inferred from the session, or override with `deliver_channel` /
`deliver_chat_id`). Do NOT rely on a separate follow-up `send_file` call inside
a scheduled job — an unattended run may end after synthesis and the audio would
never reach the user.


## Long Text Handling

Edge-TTS handles long text automatically. For very long content (>5000 chars), the script splits into chunks and concatenates audio files.

## Use Cases

- Morning briefing audio via voice channel
- Article read-aloud
- Voice notification delivery
- Language learning pronunciation
