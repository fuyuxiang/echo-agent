"""End-to-end degradation tests for inbound video understanding."""

from __future__ import annotations

from pathlib import Path

from echo_agent.agent.media.understanding import default_understanders, registry as reg_mod


def test_no_provider_no_video_understander(monkeypatch):
    """没有 vision provider 时视频不装配，understander 列表不含 VideoUnderstander。"""
    from echo_agent.agent.media.understanding.video import VideoUnderstander

    class _Cfg:
        audio_enabled = False
        video_enabled = True
        video_frame_count = 4
        video_vision_model = ""
        video_vision_prompt = "p"
        min_video_size_kb = 1.0
        max_video_size_kb = 204800

    monkeypatch.setattr(reg_mod, "_ffmpeg_available", lambda: True)
    us = default_understanders(_Cfg(), vision_provider=None)
    assert not any(isinstance(u, VideoUnderstander) for u in us)


def test_video_block_degrades_to_attachment_when_no_understander(tmp_path: Path):
    """无 understander 时视频块经真实 build_messages 降级 [附件] 类型=video，消息不丢。"""
    from echo_agent.agent.context import ContextBuilder

    cb = ContextBuilder(workspace=tmp_path, media_cache=None, understanders=[])
    media = [{"type": "video", "url": "/tmp/v.mp4", "name": "clip.mp4"}]
    msgs = cb.build_messages(history=[], current_message="看视频", media=media)
    text = msgs[-1]["content"][0]["text"]
    assert "[附件]" in text and "video" in text
