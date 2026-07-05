from pathlib import Path

import pytest

from echo_agent.agent.context import ContextBuilder
from echo_agent.agent.media.understanding.base import UnderstandResult
from echo_agent.bus.events import ContentBlock, ContentType


class _StubUnderstander:
    def __init__(self):
        self.seen = []

    def can_handle(self, block):
        btype = getattr(block.type, "value", str(block.type))
        return btype in ("audio", "voice")

    async def understand(self, path, block):
        self.seen.append(Path(path))
        return UnderstandResult(text="转写好的文本", kind="transcript")


class _StubCache:
    def __init__(self, tmp_path: Path):
        self._tmp = tmp_path

    async def download(self, url, platform):
        p = self._tmp / "dl.ogg"
        p.write_bytes(b"x" * 4096)
        return p


@pytest.mark.asyncio
async def test_audio_block_gets_transcribed(tmp_path: Path):
    u = _StubUnderstander()
    cb = ContextBuilder(tmp_path, media_cache=_StubCache(tmp_path), understanders=[u])
    block = ContentBlock(type=ContentType.VOICE, url="https://x/a.ogg", mime_type="audio/ogg")
    resolved = await cb.resolve_inbound_media([block], channel="weixin")
    assert resolved[0]["transcribed_text"] == "转写好的文本"
    assert len(u.seen) == 1


@pytest.mark.asyncio
async def test_image_block_untouched_by_understanders(tmp_path: Path):
    u = _StubUnderstander()
    cb = ContextBuilder(tmp_path, media_cache=_StubCache(tmp_path), understanders=[u])
    block = ContentBlock(type=ContentType.IMAGE, url="https://x/a.jpg", mime_type="image/jpeg")
    resolved = await cb.resolve_inbound_media([block], channel="weixin")
    assert resolved[0].get("transcribed_text", "") == ""
    assert u.seen == []  # understanders never see images


@pytest.mark.asyncio
async def test_no_understander_leaves_audio_as_reference(tmp_path: Path):
    cb = ContextBuilder(tmp_path, media_cache=_StubCache(tmp_path), understanders=[])
    block = ContentBlock(type=ContentType.VOICE, url="https://x/a.ogg", mime_type="audio/ogg")
    resolved = await cb.resolve_inbound_media([block], channel="weixin")
    assert resolved[0].get("transcribed_text", "") == ""  # message not dropped
