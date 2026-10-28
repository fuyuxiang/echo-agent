from pathlib import Path

import pytest

from echo_agent.agent.media.understanding.video import LLMVisionBackend


class _Resp:
    def __init__(self, content, finish_reason="stop"):
        self.content = content
        self.finish_reason = finish_reason


class _Provider:
    def __init__(self, resp):
        self._resp = resp
        self.calls = []

    async def chat_with_retry(self, **kwargs):
        self.calls.append(kwargs)
        return self._resp


@pytest.mark.asyncio
async def test_caption_returns_text(tmp_path: Path):
    f = tmp_path / "a.frame.1.jpg"
    f.write_bytes(b"\xff\xd8\xff jpeg")
    prov = _Provider(_Resp("一只猫在跳"))
    out = await LLMVisionBackend(prov).caption([f])
    assert out == "一只猫在跳"
    # sent multimodal content with an image_url part
    msgs = prov.calls[0]["messages"]
    parts = msgs[0]["content"]
    assert any(p.get("type") == "image_url" for p in parts)


@pytest.mark.asyncio
async def test_caption_empty_on_no_frames(tmp_path: Path):
    prov = _Provider(_Resp("unused"))
    assert await LLMVisionBackend(prov).caption([]) == ""


@pytest.mark.asyncio
async def test_caption_empty_on_error_finish(tmp_path: Path):
    f = tmp_path / "a.frame.1.jpg"
    f.write_bytes(b"\xff\xd8\xff jpeg")
    prov = _Provider(_Resp("Error: boom", finish_reason="error"))
    assert await LLMVisionBackend(prov).caption([f]) == ""


@pytest.mark.asyncio
async def test_caption_uses_configured_model(tmp_path: Path):
    f = tmp_path / "a.frame.1.jpg"
    f.write_bytes(b"\xff\xd8\xff jpeg")
    prov = _Provider(_Resp("ok"))
    await LLMVisionBackend(prov, model="gpt-4o").caption([f])
    assert prov.calls[0]["model"] == "gpt-4o"


@pytest.mark.asyncio
async def test_caption_empty_model_passed_as_none(tmp_path: Path):
    f = tmp_path / "a.frame.1.jpg"
    f.write_bytes(b"\xff\xd8\xff jpeg")
    prov = _Provider(_Resp("ok"))
    await LLMVisionBackend(prov).caption([f])  # default model=""
    assert prov.calls[0]["model"] is None


@pytest.mark.asyncio
async def test_caption_failopen_on_provider_exception(tmp_path: Path):
    f = tmp_path / "a.frame.1.jpg"
    f.write_bytes(b"\xff\xd8\xff jpeg")

    class _BoomProvider:
        async def chat_with_retry(self, **kwargs):
            raise RuntimeError("provider exploded")

    assert await LLMVisionBackend(_BoomProvider()).caption([f]) == ""

