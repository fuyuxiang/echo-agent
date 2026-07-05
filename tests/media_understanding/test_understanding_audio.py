from dataclasses import dataclass, field
from pathlib import Path

import pytest

from echo_agent.agent.media.understanding.audio import AudioTranscriber


@dataclass
class _Block:
    type: str = "voice"
    mime_type: str = ""
    url: str = ""
    metadata: dict = field(default_factory=dict)


class _StubBackend:
    def __init__(self, text="转写内容", calls=None):
        self._text = text
        self.calls = calls if calls is not None else []

    async def transcribe(self, path: Path) -> str:
        self.calls.append(path)
        return self._text


def _write(path: Path, kb: float = 5.0):
    path.write_bytes(b"x" * int(kb * 1024))


def test_can_handle_by_type_and_mime(tmp_path: Path):
    t = AudioTranscriber(_StubBackend())
    assert t.can_handle(_Block(type="voice")) is True
    assert t.can_handle(_Block(type="audio")) is True
    assert t.can_handle(_Block(type="file", mime_type="audio/ogg")) is True
    assert t.can_handle(_Block(type="image")) is False
    assert t.can_handle(_Block(type="file", mime_type="application/pdf")) is False


@pytest.mark.asyncio
async def test_understand_transcribes_and_returns_text(tmp_path: Path):
    f = tmp_path / "a.ogg"
    _write(f)
    t = AudioTranscriber(_StubBackend(text="你好"))
    r = await t.understand(f, _Block())
    assert r.text == "你好"
    assert r.kind == "transcript"


@pytest.mark.asyncio
async def test_understand_skips_too_small(tmp_path: Path):
    f = tmp_path / "tiny.ogg"
    _write(f, kb=0.2)
    backend = _StubBackend()
    t = AudioTranscriber(backend, min_size_kb=1.0)
    r = await t.understand(f, _Block())
    assert r.text == ""
    assert backend.calls == []  # never called


@pytest.mark.asyncio
async def test_understand_skips_too_large(tmp_path: Path):
    f = tmp_path / "big.ogg"
    _write(f, kb=50)
    backend = _StubBackend()
    t = AudioTranscriber(backend, max_size_kb=10)
    r = await t.understand(f, _Block())
    assert r.text == ""
    assert backend.calls == []


@pytest.mark.asyncio
async def test_understand_uses_cache_on_second_call(tmp_path: Path):
    f = tmp_path / "a.ogg"
    _write(f)
    backend = _StubBackend(text="缓存前")
    t = AudioTranscriber(backend)
    r1 = await t.understand(f, _Block())
    assert r1.text == "缓存前"
    # second transcriber with a different backend must still read the sidecar cache
    backend2 = _StubBackend(text="不该被调用")
    t2 = AudioTranscriber(backend2)
    r2 = await t2.understand(f, _Block())
    assert r2.text == "缓存前"
    assert backend2.calls == []  # served from cache


@pytest.mark.asyncio
async def test_understand_failopen_on_backend_error(tmp_path: Path):
    f = tmp_path / "a.ogg"
    _write(f)

    class _Boom:
        async def transcribe(self, path):
            raise RuntimeError("boom")

    r = await AudioTranscriber(_Boom()).understand(f, _Block())
    assert r.text == ""  # never raises


@pytest.mark.asyncio
async def test_understand_empty_result_not_cached(tmp_path: Path):
    f = tmp_path / "a.ogg"
    _write(f)
    backend = _StubBackend(text="")  # silence → empty
    t = AudioTranscriber(backend)
    await t.understand(f, _Block())
    sidecar = f.with_suffix(f.suffix + ".transcript.txt")
    assert not sidecar.exists()  # empty transcript is not persisted
