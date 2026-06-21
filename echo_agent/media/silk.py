"""Encode arbitrary audio into Tencent-flavoured SILK v3 for Weixin voice.

Pipeline: source audio --ffmpeg--> PCM s16le mono 24kHz --pilk--> .silk.
ffmpeg is resolved from imageio-ffmpeg's bundled binary first, then from PATH.
Duration is derived from the PCM byte count (no extra probe).
"""

from __future__ import annotations

import asyncio
import os
import shutil
import tempfile

from loguru import logger

_PCM_RATE = 24000  # Weixin voice sample rate
_BYTES_PER_SAMPLE = 2  # s16le → 2 bytes/sample, mono


def _resolve_ffmpeg() -> str:
    """Return an ffmpeg executable path: bundled (imageio-ffmpeg) then PATH."""
    try:
        import imageio_ffmpeg

        exe = imageio_ffmpeg.get_ffmpeg_exe()
        if exe and os.path.exists(exe):
            return exe
    except Exception as exc:  # imageio-ffmpeg missing or binary unavailable
        logger.debug("imageio-ffmpeg unavailable: {}", exc)
    system = shutil.which("ffmpeg")
    if system:
        return system
    raise FileNotFoundError("no ffmpeg available (imageio-ffmpeg or PATH)")


async def _ffmpeg_to_pcm(src_path: str, pcm_path: str) -> None:
    ffmpeg = _resolve_ffmpeg()
    proc = await asyncio.create_subprocess_exec(
        ffmpeg, "-i", src_path,
        "-f", "s16le", "-acodec", "pcm_s16le",
        "-ar", str(_PCM_RATE), "-ac", "1", "-y", pcm_path,
        stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE,
    )
    _, stderr = await proc.communicate()
    if proc.returncode != 0:
        raise RuntimeError(
            f"ffmpeg failed (rc={proc.returncode}): {stderr.decode(errors='ignore')[:300]}"
        )


async def encode_to_silk(src_path: str) -> tuple[str, int]:
    """Audio file → (silk_path, duration_ms). Raises on any encode failure.

    Caller owns the returned .silk temp file and must delete it.
    """
    import pilk

    if not os.path.exists(src_path):
        raise FileNotFoundError(f"audio source not found: {src_path}")

    pcm_fd, pcm_path = tempfile.mkstemp(suffix=".pcm")
    os.close(pcm_fd)
    silk_fd, silk_path = tempfile.mkstemp(suffix=".silk")
    os.close(silk_fd)
    try:
        await _ffmpeg_to_pcm(src_path, pcm_path)
        # pilk.encode is CPU-bound and synchronous → run off the event loop.
        await asyncio.to_thread(
            pilk.encode, pcm_path, silk_path, pcm_rate=_PCM_RATE, tencent=True
        )
        pcm_bytes = os.path.getsize(pcm_path)
        duration_ms = int(pcm_bytes / (_BYTES_PER_SAMPLE * _PCM_RATE) * 1000)
        return silk_path, duration_ms
    except Exception:
        if os.path.exists(silk_path):
            os.unlink(silk_path)
        raise
    finally:
        if os.path.exists(pcm_path):
            os.unlink(pcm_path)
