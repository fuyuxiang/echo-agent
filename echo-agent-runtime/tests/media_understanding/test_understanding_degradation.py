from pathlib import Path

import pytest

from echo_agent.agent.context import ContextBuilder
from echo_agent.agent.media.understanding import default_understanders
from echo_agent.agent.media.understanding import registry as reg_mod
from echo_agent.bus.events import ContentBlock, ContentType


class _Cfg:
    audio_enabled = True
    audio_provider = "auto"
    min_audio_size_kb = 1.0
    max_audio_size_kb = 25000
    local_model_size = "base"


class _StubCache:
    def __init__(self, tmp_path):
        self._tmp = tmp_path

    async def download(self, url, platform):
        p = self._tmp / "dl.ogg"
        p.write_bytes(b"x" * 4096)
        return p


@pytest.mark.asyncio
async def test_no_provider_degrades_to_reference_message_not_dropped(tmp_path: Path, monkeypatch):
    # neither cloud key nor local lib → empty understander list
    monkeypatch.setattr(reg_mod, "_local_available", lambda: False)
    understanders = default_understanders(_Cfg(), transcription_api_key="")
    assert understanders == []
    cb = ContextBuilder(tmp_path, media_cache=_StubCache(tmp_path), understanders=understanders)
    block = ContentBlock(type=ContentType.VOICE, url="https://x/a.ogg", mime_type="audio/ogg")
    resolved = await cb.resolve_inbound_media([block], channel="weixin")
    # message survives, just no transcript
    assert resolved and resolved[0].get("transcribed_text", "") == ""
    msgs = cb.build_messages(history=[], current_message="（语音）", media=resolved)
    rendered = msgs[-1]["content"]
    text = rendered[0]["text"] if isinstance(rendered, list) else rendered
    assert "[附件]" in text
