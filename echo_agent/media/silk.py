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

from echo_agent.agent.proc_lifecycle import subprocess_kwargs, terminate_tree

_PCM_RATE = 24000  # Weixin voice sample rate
_BYTES_PER_SAMPLE = 2  # s16le → 2 bytes/sample, mono
# Upper bound for one ffmpeg transcode. Voice messages are short, so this is
# generous; it exists because ffmpeg can stall indefinitely on a truncated or
# malformed input, and nothing upstream bounds this call — channels.weixin
# awaits encode_to_silk directly, so a wedged ffmpeg would hang voice sending
# and leak the process.
_FFMPEG_TIMEOUT = 120.0


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
        **subprocess_kwargs(),
    )
    try:
        _, stderr = await asyncio.wait_for(proc.communicate(), timeout=_FFMPEG_TIMEOUT)
    except (asyncio.TimeoutError, TimeoutError):
        await terminate_tree(proc)
        raise RuntimeError(f"ffmpeg timed out after {_FFMPEG_TIMEOUT}s") from None
    except asyncio.CancelledError:
        # Caller went away (shutdown, send aborted): don't leave ffmpeg running.
        await terminate_tree(proc)
        raise
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
    ok = False
    try:
        await _ffmpeg_to_pcm(src_path, pcm_path)
        # pilk.encode is CPU-bound and synchronous → run off the event loop.
        await asyncio.to_thread(
            pilk.encode, pcm_path, silk_path, pcm_rate=_PCM_RATE, tencent=True
        )
        pcm_bytes = os.path.getsize(pcm_path)
        duration_ms = int(pcm_bytes / (_BYTES_PER_SAMPLE * _PCM_RATE) * 1000)
        ok = True
        return silk_path, duration_ms
    finally:
        # Both temp files are cleaned on every non-success exit, cancellation
        # included. The previous `except Exception` missed CancelledError (a
        # BaseException since 3.8) — and cancellation is a real path here, both
        # awaits can raise it — so an aborted send left an empty .silk behind
        # every time. `ok` rather than exception matching: only a completed
        # encode hands silk_path to the caller, so only then must we keep it.
        if not ok and os.path.exists(silk_path):
            os.unlink(silk_path)
        if os.path.exists(pcm_path):
            os.unlink(pcm_path)
