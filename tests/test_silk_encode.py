"""Tests for SILK encoding used by the Weixin voice channel.

Offline: generates a short tone with ffmpeg, encodes to Tencent SILK v3,
asserts the file is non-empty, the duration is sane, and the header carries
the SILK magic. Skips cleanly when ffmpeg is unavailable.
"""

from __future__ import annotations

import subprocess

import pytest

from echo_agent.media import silk


def _ffmpeg_available() -> bool:
    try:
        return bool(silk._resolve_ffmpeg())
    except Exception:
        return False


def _pilk_available() -> bool:
    try:
        import pilk  # noqa: F401

        return True
    except Exception:
        return False


pytestmark = pytest.mark.skipif(
    not (_ffmpeg_available() and _pilk_available()),
    reason="ffmpeg and pilk required for SILK encoding",
)


def _make_wav(path: str, seconds: float) -> None:
    ffmpeg = silk._resolve_ffmpeg()
    subprocess.run(
        [ffmpeg, "-f", "lavfi", "-i", f"sine=frequency=440:duration={seconds}",
         "-ar", "24000", "-ac", "1", "-y", path],
        check=True, capture_output=True,
    )


@pytest.mark.asyncio
async def test_encode_to_silk_produces_tencent_silk(tmp_path):
    src = tmp_path / "tone.wav"
    _make_wav(str(src), 1.0)
    silk_path, duration_ms = await silk.encode_to_silk(str(src))
    data = open(silk_path, "rb").read()
    assert len(data) > 0
    # Tencent SILK v3 header begins with b"\x02#!SILK_V3"
    assert b"#!SILK_V3" in data[:12]
    # ~1s tone → duration within a tolerant band
    assert 700 <= duration_ms <= 1300


@pytest.mark.asyncio
async def test_encode_to_silk_raises_on_missing_source(tmp_path):
    with pytest.raises(Exception):
        await silk.encode_to_silk(str(tmp_path / "nope.mp3"))


# ── ffmpeg 转码有超时上界并回收进程组 ────────────────────────────────────────


@pytest.mark.asyncio
async def test_wedged_ffmpeg_times_out_and_is_reaped(tmp_path, monkeypatch):
    """卡死的 ffmpeg 必须超时报错并被回收。

    原实现是裸 `await proc.communicate()`,而 channels/weixin.py 直接 await
    encode_to_silk 没有外层兜底:ffmpeg 遇到截断/畸形音频长时间不返回时,会挂住
    微信语音发送路径,卡住的进程也不回收。
    """
    import asyncio

    monkeypatch.setattr(silk, "_FFMPEG_TIMEOUT", 0.3)
    monkeypatch.setattr(silk, "_resolve_ffmpeg", lambda: "/bin/sh")
    real_exec = asyncio.create_subprocess_exec
    spawned = []

    async def _fake_exec(*args, **kwargs):
        proc = await real_exec(
            "sleep", "30",
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE,
            start_new_session=kwargs.get("start_new_session", False),
        )
        spawned.append(proc)
        return proc

    monkeypatch.setattr(silk.asyncio, "create_subprocess_exec", _fake_exec)

    with pytest.raises(RuntimeError, match="timed out"):
        await silk._ffmpeg_to_pcm(str(tmp_path / "in.mp3"), str(tmp_path / "out.pcm"))
    assert spawned and spawned[0].returncode is not None, "超时后必须回收子进程"


@pytest.mark.asyncio
async def test_cancelled_ffmpeg_is_reaped(tmp_path, monkeypatch):
    """调用方被取消时 ffmpeg 也必须回收,不能变孤儿。"""
    import asyncio

    monkeypatch.setattr(silk, "_resolve_ffmpeg", lambda: "/bin/sh")
    real_exec = asyncio.create_subprocess_exec
    spawned = []

    async def _fake_exec(*args, **kwargs):
        proc = await real_exec(
            "sleep", "30",
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE,
            start_new_session=kwargs.get("start_new_session", False),
        )
        spawned.append(proc)
        return proc

    monkeypatch.setattr(silk.asyncio, "create_subprocess_exec", _fake_exec)

    task = asyncio.create_task(
        silk._ffmpeg_to_pcm(str(tmp_path / "in.mp3"), str(tmp_path / "out.pcm"))
    )
    await asyncio.sleep(0.2)
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert spawned and spawned[0].returncode is not None, "取消后必须回收子进程"
