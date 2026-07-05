from pathlib import Path

import pytest

from echo_agent.agent.media.understanding import audio as audio_mod
from echo_agent.agent.media.understanding.audio import (
    CloudWhisperBackend,
    LocalWhisperBackend,
    _cloud_available,
)


def test_cloud_available_reflects_api_key():
    assert _cloud_available("sk-x") is True
    assert _cloud_available("") is False


@pytest.mark.asyncio
async def test_cloud_transcribe_returns_text_on_200(tmp_path: Path, monkeypatch):
    f = tmp_path / "a.wav"
    f.write_bytes(b"RIFFfake")

    class _Resp:
        status = 200

        async def json(self):
            return {"text": "  你好世界  "}

        async def text(self):
            return ""

        async def __aenter__(self):
            return self

        async def __aexit__(self, *a):
            return False

    class _Session:
        def __init__(self, *a, **k):
            pass

        def post(self, *a, **k):
            return _Resp()

        async def __aenter__(self):
            return self

        async def __aexit__(self, *a):
            return False

    monkeypatch.setattr(audio_mod.aiohttp, "ClientSession", _Session)
    out = await CloudWhisperBackend("sk-x").transcribe(f)
    assert out == "你好世界"  # stripped


@pytest.mark.asyncio
async def test_cloud_transcribe_failopen_on_non_200(tmp_path: Path, monkeypatch):
    f = tmp_path / "a.wav"
    f.write_bytes(b"RIFFfake")

    class _Resp:
        status = 500

        async def json(self):
            return {}

        async def text(self):
            return "boom"

        async def __aenter__(self):
            return self

        async def __aexit__(self, *a):
            return False

    class _Session:
        def __init__(self, *a, **k):
            pass

        def post(self, *a, **k):
            return _Resp()

        async def __aenter__(self):
            return self

        async def __aexit__(self, *a):
            return False

    monkeypatch.setattr(audio_mod.aiohttp, "ClientSession", _Session)

    records: list[str] = []
    handler_id = audio_mod.logger.add(lambda m: records.append(m.record["level"].name), level="WARNING")
    try:
        out = await CloudWhisperBackend("sk-x").transcribe(f)
    finally:
        audio_mod.logger.remove(handler_id)
    assert out == ""
    assert "WARNING" in records


@pytest.mark.asyncio
async def test_cloud_transcribe_failopen_on_missing_file(tmp_path: Path):
    out = await CloudWhisperBackend("sk-x").transcribe(tmp_path / "nope.wav")
    assert out == ""


@pytest.mark.asyncio
async def test_local_transcribe_joins_segments(tmp_path: Path, monkeypatch):
    f = tmp_path / "a.wav"
    f.write_bytes(b"RIFFfake")

    class _Seg:
        def __init__(self, text):
            self.text = text

    class _FakeModel:
        def __init__(self, *a, **k):
            pass

        def transcribe(self, path, **k):
            return [_Seg(" 你好"), _Seg(" 世界")], {"language": "zh"}

    # LocalWhisperBackend imports WhisperModel lazily from faster_whisper;
    # patch the resolver to return our fake class.
    monkeypatch.setattr(
        LocalWhisperBackend, "_load_model_cls", staticmethod(lambda: _FakeModel)
    )
    out = await LocalWhisperBackend().transcribe(f)
    assert out == "你好 世界"
