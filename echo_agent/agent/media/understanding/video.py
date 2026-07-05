from __future__ import annotations

import asyncio
import base64
import re
import subprocess
from pathlib import Path
from typing import Any, Protocol

from loguru import logger

from echo_agent.agent.media.understanding.base import UnderstandResult

# Matches the "Duration: HH:MM:SS.ss" line ffmpeg prints to stderr for any input.
_DURATION_RE = re.compile(r"Duration:\s*(\d+):(\d+):(\d+(?:\.\d+)?)")


def _get_ffmpeg_exe() -> str:
    import imageio_ffmpeg
    return imageio_ffmpeg.get_ffmpeg_exe()


def _ffmpeg_available() -> bool:
    try:
        return bool(_get_ffmpeg_exe())
    except Exception:
        return False


def _probe_duration(exe: str, path: Path) -> float | None:
    """Best-effort media duration in seconds; None if it can't be determined.

    Runs `ffmpeg -i <path>` and scrapes the "Duration:" line from stderr. Never
    raises: any failure returns None so callers can fall back (fail-open).
    """
    try:
        proc = subprocess.run(
            [exe, "-i", str(path)],
            capture_output=True, timeout=60, check=False,
        )
    except Exception:
        return None
    stderr = proc.stderr
    if isinstance(stderr, (bytes, bytearray)):
        stderr = stderr.decode("utf-8", "replace")
    m = _DURATION_RE.search(stderr or "")
    if not m:
        return None
    total = int(m.group(1)) * 3600 + int(m.group(2)) * 60 + float(m.group(3))
    return total if total > 0 else None


def extract_frames(path: Path, count: int, *, out_dir: Path | None = None) -> list[Path]:
    """Uniformly sample `count` frames to jpg files; [] on failure (fail-open)."""
    if count < 1:
        return []
    target_dir = out_dir or path.parent
    pattern = str(target_dir / f"{path.stem}.frame.%d.jpg")
    try:
        exe = _get_ffmpeg_exe()
        duration = _probe_duration(exe, path)
        if duration and duration > 0:
            # True uniform sampling: emit one frame every duration/count seconds
            # (fps = count/duration) so the `count` frames are spread evenly over
            # the whole clip; -frames:v caps the count against rounding overrun.
            vf = f"fps={count}/{duration}"
        else:
            # Duration unknown: fall back to thumbnail (one representative frame
            # per batch) so we still return frames rather than nothing. Frames may
            # skew toward the start, but the call stays non-empty (fail-open).
            vf = "thumbnail"
        cmd = [
            exe, "-y", "-i", str(path),
            "-vf", vf,
            "-frames:v", str(count),
            "-vsync", "0",
            pattern,
        ]
        subprocess.run(cmd, capture_output=True, timeout=120, check=False)
    except Exception as e:  # fail-open
        logger.warning("frame extraction failed (fail-open): {}", e)
        return []
    frames = sorted(
        p for p in target_dir.glob(f"{path.stem}.frame.*.jpg") if p.is_file()
    )
    return frames


def extract_audio_track(path: Path, *, out_path: Path | None = None) -> Path | None:
    """Extract the audio track to a wav file; None if no track / failure (fail-open)."""
    out = out_path or path.with_suffix(path.suffix + ".track.wav")
    try:
        exe = _get_ffmpeg_exe()
        cmd = [exe, "-y", "-i", str(path), "-vn", "-acodec", "pcm_s16le",
               "-ar", "16000", "-ac", "1", str(out)]
        subprocess.run(cmd, capture_output=True, timeout=120, check=False)
    except Exception as e:  # fail-open
        logger.warning("audio track extraction failed (fail-open): {}", e)
        return None
    if out.exists() and out.stat().st_size > 0:
        return out
    return None


class VisionBackend(Protocol):
    async def caption(self, frame_paths: list[Path]) -> str: ...


def _encode_frame(path: Path) -> dict[str, Any]:
    from echo_agent.channels.qqbot_media import image_mime_for
    mime = image_mime_for(str(path))
    b64 = base64.b64encode(path.read_bytes()).decode()
    return {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}}


class LLMVisionBackend:
    """Caption video frames via a multimodal LLM provider."""

    def __init__(self, provider: Any, *, model: str = "",
                 prompt: str = "简要描述这段视频的画面内容。") -> None:
        self._provider = provider
        self._model = model
        self._prompt = prompt

    async def caption(self, frame_paths: list[Path]) -> str:
        frames = [p for p in frame_paths if p.is_file()]
        if not frames:
            return ""
        try:
            content: list[dict[str, Any]] = [_encode_frame(p) for p in frames]
            content.append({"type": "text", "text": self._prompt})
            messages = [{"role": "user", "content": content}]
            resp = await self._provider.chat_with_retry(messages=messages, model=self._model or None)
            if getattr(resp, "finish_reason", "") == "error":
                logger.warning("vision caption failed (fail-open): {}", resp.content)
                return ""
            return (resp.content or "").strip()
        except Exception as e:  # fail-open
            logger.warning("vision caption failed (fail-open): {}", e)
            return ""


_VIDEO_TYPES = {"video"}

_ffmpeg_semaphore = asyncio.Semaphore(2)


def set_ffmpeg_concurrency(n: int) -> None:
    """Reset the process-wide cap on concurrent ffmpeg subprocesses. ffmpeg is a
    whole-machine resource, so the limit is module-level, not per-instance."""
    global _ffmpeg_semaphore
    _ffmpeg_semaphore = asyncio.Semaphore(max(1, n))


class VideoUnderstander:
    """MediaUnderstanding impl: caption frames + transcribe audio track."""

    def __init__(self, vision: Any, transcribe: Any | None, *,
                 frame_count: int = 4, min_size_kb: float = 1.0,
                 max_size_kb: int = 204800, ffmpeg_concurrency: int = 2) -> None:
        self._vision = vision
        self._transcribe = transcribe
        self._frame_count = frame_count
        self._min_size_kb = min_size_kb
        self._max_size_kb = max_size_kb
        set_ffmpeg_concurrency(ffmpeg_concurrency)

    def can_handle(self, block: Any) -> bool:
        btype = getattr(getattr(block, "type", None), "value", None) or str(getattr(block, "type", ""))
        if btype in _VIDEO_TYPES:
            return True
        mime = getattr(block, "mime_type", "") or ""
        return mime.startswith("video/")

    @staticmethod
    def _sidecar(path: Path) -> Path:
        return path.with_suffix(path.suffix + ".video.txt")

    async def understand(self, path: Path, block: Any) -> UnderstandResult:
        empty = UnderstandResult(text="", kind="video")
        temp_files: list[Path] = []
        try:
            if not path.exists():
                return empty
            sidecar = self._sidecar(path)
            if sidecar.exists():
                cached = sidecar.read_text(encoding="utf-8")
                if cached:
                    return UnderstandResult(text=cached, kind="video", metadata={"cached": "1"})
            size_kb = path.stat().st_size / 1024
            if size_kb < self._min_size_kb or size_kb > self._max_size_kb:
                return empty
            # frame path (vision) — ffmpeg runs off the event loop, capped by the
            # process-wide semaphore so N concurrent videos can't fork N ffmpegs.
            caption = ""
            async with _ffmpeg_semaphore:
                frames = await asyncio.to_thread(
                    extract_frames, path, self._frame_count
                )
            temp_files.extend(frames)
            if frames:
                caption = await self._vision.caption(frames)
            # audio track path (transcribe)
            transcript = ""
            if self._transcribe is not None:
                async with _ffmpeg_semaphore:
                    track = await asyncio.to_thread(extract_audio_track, path)
                if track is not None:
                    temp_files.append(track)
                    transcript = await self._transcribe.transcribe(track)
            # assemble: any track present → inject
            parts = []
            if caption:
                parts.append(f"画面：{caption}")
            if transcript:
                parts.append(f"语音：{transcript}")
            text = "\n".join(parts)
            if not text:
                return empty
            try:
                sidecar.write_text(text, encoding="utf-8")
            except Exception as e:
                logger.debug("video transcript cache write failed (ignored): {}", e)
            return UnderstandResult(text=text, kind="video")
        except Exception as e:  # fail-open
            logger.warning("video understand failed (fail-open): {}", e)
            return empty
        finally:
            for f in temp_files:
                try:
                    f.unlink(missing_ok=True)
                except Exception:
                    pass
