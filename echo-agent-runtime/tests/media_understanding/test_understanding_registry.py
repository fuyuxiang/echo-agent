from echo_agent.agent.media.understanding import registry as reg_mod
from echo_agent.agent.media.understanding.registry import default_understanders
from echo_agent.agent.media.understanding.audio import (
    AudioTranscriber,
    CloudWhisperBackend,
    LocalWhisperBackend,
)
from echo_agent.agent.media.understanding.video import VideoUnderstander


class _Cfg:
    def __init__(self, enabled=True, provider="auto",
                 min_audio_size_kb=1.0, max_audio_size_kb=25000, local_model_size="base",
                 transcription_base_url="https://api.groq.com/openai/v1",
                 transcription_model="whisper-large-v3",
                 video_enabled=True, video_frame_count=4, video_vision_model="",
                 video_vision_prompt="p", min_video_size_kb=1.0, max_video_size_kb=204800):
        self.audio_enabled = enabled
        self.audio_provider = provider
        self.min_audio_size_kb = min_audio_size_kb
        self.max_audio_size_kb = max_audio_size_kb
        self.local_model_size = local_model_size
        self.transcription_base_url = transcription_base_url
        self.transcription_model = transcription_model
        self.video_enabled = video_enabled
        self.video_frame_count = video_frame_count
        self.video_vision_model = video_vision_model
        self.video_vision_prompt = video_vision_prompt
        self.min_video_size_kb = min_video_size_kb
        self.max_video_size_kb = max_video_size_kb


def test_disabled_returns_empty():
    assert default_understanders(_Cfg(enabled=False), transcription_api_key="sk") == []


def test_auto_prefers_cloud_when_key_present(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: True)
    us = default_understanders(_Cfg(provider="auto"), transcription_api_key="sk-x")
    assert len(us) == 1
    assert isinstance(us[0], AudioTranscriber)
    assert isinstance(us[0]._backend, CloudWhisperBackend)


def test_auto_falls_back_to_local_without_key(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: True)
    us = default_understanders(_Cfg(provider="auto"), transcription_api_key="")
    assert len(us) == 1
    assert isinstance(us[0]._backend, LocalWhisperBackend)


def test_auto_empty_when_nothing_available(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: False)
    assert default_understanders(_Cfg(provider="auto"), transcription_api_key="") == []


def test_cloud_forced_but_no_key_is_empty(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: True)
    assert default_understanders(_Cfg(provider="cloud"), transcription_api_key="") == []


def test_local_forced_unavailable_is_empty(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: False)
    assert default_understanders(_Cfg(provider="local"), transcription_api_key="sk-x") == []


def test_cloud_backend_uses_configured_endpoint(monkeypatch):
    cfg = _Cfg(
        provider="cloud",
        transcription_base_url="https://api.openai.com/v1/",
        transcription_model="whisper-1",
    )
    us = default_understanders(cfg, transcription_api_key="sk-x")
    assert len(us) == 1
    backend = us[0]._backend
    assert isinstance(backend, CloudWhisperBackend)
    assert backend._base_url == "https://api.openai.com/v1"  # trailing slash stripped
    assert backend._model == "whisper-1"


class _FakeProvider:
    async def chat_with_retry(self, **kwargs):
        raise AssertionError("not called in assembly")


def test_video_assembled_when_enabled_and_available(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: False)
    monkeypatch.setattr(reg_mod, "_ffmpeg_available", lambda: True)
    us = default_understanders(
        _Cfg(enabled=False, video_enabled=True),
        transcription_api_key="",
        vision_provider=_FakeProvider(),
    )
    assert any(isinstance(u, VideoUnderstander) for u in us)


def test_video_skipped_without_ffmpeg(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: False)
    monkeypatch.setattr(reg_mod, "_ffmpeg_available", lambda: False)
    us = default_understanders(
        _Cfg(enabled=False, video_enabled=True),
        vision_provider=_FakeProvider(),
    )
    assert not any(isinstance(u, VideoUnderstander) for u in us)


def test_video_skipped_without_provider(monkeypatch):
    monkeypatch.setattr(reg_mod, "_ffmpeg_available", lambda: True)
    us = default_understanders(
        _Cfg(enabled=False, video_enabled=True),
        vision_provider=None,
    )
    assert not any(isinstance(u, VideoUnderstander) for u in us)


def test_video_skipped_when_disabled(monkeypatch):
    monkeypatch.setattr(reg_mod, "_ffmpeg_available", lambda: True)
    us = default_understanders(
        _Cfg(enabled=False, video_enabled=False),
        vision_provider=_FakeProvider(),
    )
    assert not any(isinstance(u, VideoUnderstander) for u in us)


def test_audio_and_video_both_assembled(monkeypatch):
    monkeypatch.setattr(reg_mod, "_local_available", lambda: True)
    monkeypatch.setattr(reg_mod, "_ffmpeg_available", lambda: True)
    us = default_understanders(
        _Cfg(enabled=True, provider="auto", video_enabled=True),
        transcription_api_key="",
        vision_provider=_FakeProvider(),
    )
    assert isinstance(us[0], AudioTranscriber)
    assert any(isinstance(u, VideoUnderstander) for u in us)
