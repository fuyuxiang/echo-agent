import asyncio
import threading
import pytest
from echo_agent.agent.media.understanding import video as vmod
from echo_agent.agent.media.understanding.video import VideoUnderstander


class _Vision:
    async def caption(self, frames):
        return "画面描述"


@pytest.mark.asyncio
async def test_understand_does_not_block_event_loop(tmp_path, monkeypatch):
    f = tmp_path / "clip.mp4"
    f.write_bytes(b"x" * 4096)

    def slow_extract_frames(path, count, *, out_dir=None):
        import time as _t
        _t.sleep(0.3)  # simulate blocking ffmpeg
        return []

    monkeypatch.setattr(vmod, "extract_frames", slow_extract_frames)
    monkeypatch.setattr(vmod, "extract_audio_track", lambda path, **k: None)

    ticks = {"n": 0}
    async def ticker():
        for _ in range(20):
            ticks["n"] += 1
            await asyncio.sleep(0.02)

    u = VideoUnderstander(_Vision(), None)
    t = asyncio.create_task(ticker())
    await u.understand(f, block=None)
    await t
    # if to_thread worked, the ticker advanced during the 0.3s blocking extract
    assert ticks["n"] >= 5


@pytest.mark.asyncio
async def test_ffmpeg_concurrency_capped(tmp_path, monkeypatch):
    vmod.set_ffmpeg_concurrency(2)
    peak = {"cur": 0, "max": 0}
    # tracking_extract runs in a worker thread via asyncio.to_thread, so use a
    # threading.Lock (not asyncio.Lock) to synchronize the peak-counter mutations.
    lock = threading.Lock()

    def tracking_extract(path, count, *, out_dir=None):
        import time as _t
        # synchronize the increment / max-update across worker threads
        with lock:
            peak["cur"] += 1
            peak["max"] = max(peak["max"], peak["cur"])
        # sleep OUTSIDE the lock so workers actually overlap and the cap is tested
        _t.sleep(0.2)
        with lock:
            peak["cur"] -= 1
        return []

    monkeypatch.setattr(vmod, "extract_frames", tracking_extract)
    monkeypatch.setattr(vmod, "extract_audio_track", lambda path, **k: None)

    files = []
    for i in range(4):
        f = tmp_path / f"c{i}.mp4"
        f.write_bytes(b"x" * 4096)
        files.append(f)

    u = VideoUnderstander(_Vision(), None, ffmpeg_concurrency=2)
    await asyncio.gather(*(u.understand(f, block=None) for f in files))
    assert peak["max"] <= 2
