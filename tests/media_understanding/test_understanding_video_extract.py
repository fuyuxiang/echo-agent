import subprocess
from pathlib import Path

from echo_agent.agent.media.understanding import video as video_mod
from echo_agent.agent.media.understanding.video import (
    _ffmpeg_available,
    extract_audio_track,
    extract_frames,
)


def test_ffmpeg_available_true_when_importable(monkeypatch):
    monkeypatch.setattr(video_mod, "_get_ffmpeg_exe", lambda: "/usr/bin/ffmpeg")
    assert _ffmpeg_available() is True


def test_ffmpeg_available_false_on_import_error(monkeypatch):
    def _boom():
        raise RuntimeError("no ffmpeg")
    monkeypatch.setattr(video_mod, "_get_ffmpeg_exe", _boom)
    assert _ffmpeg_available() is False


def test_extract_frames_returns_paths_on_success(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(video_mod, "_get_ffmpeg_exe", lambda: "ffmpeg")
    calls = []

    def _fake_run(cmd, **kwargs):
        # ffmpeg writes frame files; simulate by creating them
        out_pattern = cmd[-1]
        for i in range(1, 4):
            Path(out_pattern.replace("%d", str(i))).write_bytes(b"jpeg")
        calls.append(cmd)
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    monkeypatch.setattr(video_mod.subprocess, "run", _fake_run)
    frames = extract_frames(tmp_path / "v.mp4", 3, out_dir=tmp_path)
    assert len(frames) == 3
    assert all(p.exists() for p in frames)


def test_extract_frames_failopen_on_ffmpeg_error(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(video_mod, "_get_ffmpeg_exe", lambda: "ffmpeg")

    def _boom(cmd, **kwargs):
        raise subprocess.SubprocessError("ffmpeg crashed")

    monkeypatch.setattr(video_mod.subprocess, "run", _boom)
    assert extract_frames(tmp_path / "v.mp4", 3, out_dir=tmp_path) == []


def test_extract_audio_track_returns_path_on_success(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(video_mod, "_get_ffmpeg_exe", lambda: "ffmpeg")

    def _fake_run(cmd, **kwargs):
        out = cmd[-1]
        Path(out).write_bytes(b"RIFFwav")
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    monkeypatch.setattr(video_mod.subprocess, "run", _fake_run)
    out = extract_audio_track(tmp_path / "v.mp4", out_path=tmp_path / "a.wav")
    assert out is not None and out.exists()


def test_extract_audio_track_none_on_error(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(video_mod, "_get_ffmpeg_exe", lambda: "ffmpeg")

    def _boom(cmd, **kwargs):
        raise subprocess.SubprocessError("no audio stream")

    monkeypatch.setattr(video_mod.subprocess, "run", _boom)
    assert extract_audio_track(tmp_path / "v.mp4", out_path=tmp_path / "a.wav") is None
