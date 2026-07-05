from __future__ import annotations

import subprocess
from pathlib import Path

from loguru import logger


def _get_ffmpeg_exe() -> str:
    import imageio_ffmpeg
    return imageio_ffmpeg.get_ffmpeg_exe()


def _ffmpeg_available() -> bool:
    try:
        return bool(_get_ffmpeg_exe())
    except Exception:
        return False


def extract_frames(path: Path, count: int, *, out_dir: Path | None = None) -> list[Path]:
    """Uniformly sample `count` frames to jpg files; [] on failure (fail-open)."""
    if count < 1:
        return []
    target_dir = out_dir or path.parent
    pattern = str(target_dir / f"{path.stem}.frame.%d.jpg")
    try:
        exe = _get_ffmpeg_exe()
        # Uniform sampling: the `thumbnail=n=` filter picks a representative frame
        # per N-frame batch; combined with -frames:v it caps the total. This avoids
        # needing to probe duration first while spreading picks across the clip.
        cmd = [
            exe, "-y", "-i", str(path),
            "-vf", "thumbnail",
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
