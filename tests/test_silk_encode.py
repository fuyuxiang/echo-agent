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


pytestmark = pytest.mark.skipif(
    not _ffmpeg_available(), reason="ffmpeg not available for SILK encoding"
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
