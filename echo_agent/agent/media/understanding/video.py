from __future__ import annotations

import re
import subprocess
from pathlib import Path

from loguru import logger

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
