from pathlib import Path

import pytest

from echo_agent.agent.media.understanding import video as video_mod
from echo_agent.agent.media.understanding.video import VideoUnderstander


class _Block:
    def __init__(self, type="video", mime_type="video/mp4"):
        self.type = type
        self.mime_type = mime_type


class _Vision:
    def __init__(self, text):
        self._text = text

    async def caption(self, frames):
        return self._text


class _Transcribe:
    def __init__(self, text):
        self._text = text

    async def transcribe(self, path):
        return self._text


def _patch_extract(monkeypatch, frames, audio):
    monkeypatch.setattr(video_mod, "extract_frames", lambda p, c, **k: frames)
    monkeypatch.setattr(video_mod, "extract_audio_track", lambda p, **k: audio)


def test_can_handle_video_type():
    u = VideoUnderstander(_Vision(""), None)
    assert u.can_handle(_Block(type="video")) is True
    assert u.can_handle(_Block(type="image", mime_type="image/png")) is False


def test_can_handle_video_mime():
    u = VideoUnderstander(_Vision(""), None)
    assert u.can_handle(_Block(type="other", mime_type="video/webm")) is True


@pytest.mark.asyncio
async def test_both_tracks_present(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 2048)
    _patch_extract(monkeypatch, [tmp_path / "f.jpg"], tmp_path / "a.wav")
    u = VideoUnderstander(_Vision("一只猫"), _Transcribe("你好"))
    res = await u.understand(v, _Block())
    assert res.kind == "video"
    assert res.text == "画面：一只猫\n语音：你好"


@pytest.mark.asyncio
async def test_only_caption(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 2048)
    _patch_extract(monkeypatch, [tmp_path / "f.jpg"], None)
    u = VideoUnderstander(_Vision("一只猫"), _Transcribe(""))
    res = await u.understand(v, _Block())
    assert res.text == "画面：一只猫"


@pytest.mark.asyncio
async def test_only_transcript(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 2048)
    _patch_extract(monkeypatch, [], tmp_path / "a.wav")
    u = VideoUnderstander(_Vision(""), _Transcribe("你好"))
    res = await u.understand(v, _Block())
    assert res.text == "语音：你好"


@pytest.mark.asyncio
async def test_both_empty_degrades(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 2048)
    _patch_extract(monkeypatch, [], None)
    u = VideoUnderstander(_Vision(""), _Transcribe(""))
    res = await u.understand(v, _Block())
    assert res.text == ""
    assert res.kind == "video"


@pytest.mark.asyncio
async def test_preflight_oversize_skips(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 4096)
    called = {"v": False}

    async def _boom(frames):
        called["v"] = True
        return "should not run"

    u = VideoUnderstander(type("V", (), {"caption": _boom})(), None, max_size_kb=1)
    res = await u.understand(v, _Block())
    assert res.text == ""
    assert called["v"] is False


@pytest.mark.asyncio
async def test_sidecar_cache_hit(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 2048)
    sidecar = v.with_suffix(v.suffix + ".video.txt")
    sidecar.write_text("缓存内容", encoding="utf-8")
    called = {"v": False}

    async def _boom(frames):
        called["v"] = True
        return "x"

    u = VideoUnderstander(type("V", (), {"caption": _boom})(), None)
    res = await u.understand(v, _Block())
    assert res.text == "缓存内容"
    assert called["v"] is False


@pytest.mark.asyncio
async def test_no_transcribe_backend_caption_only(tmp_path: Path, monkeypatch):
    v = tmp_path / "v.mp4"
    v.write_bytes(b"x" * 2048)
    _patch_extract(monkeypatch, [tmp_path / "f.jpg"], tmp_path / "a.wav")
    u = VideoUnderstander(_Vision("画面描述"), None)  # transcribe=None
    res = await u.understand(v, _Block())
    assert res.text == "画面：画面描述"
