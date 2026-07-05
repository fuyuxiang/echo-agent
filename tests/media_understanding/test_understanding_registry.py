from echo_agent.agent.media.understanding import registry as reg_mod
from echo_agent.agent.media.understanding.registry import default_understanders
from echo_agent.agent.media.understanding.audio import (
    AudioTranscriber,
    CloudWhisperBackend,
    LocalWhisperBackend,
)


class _Cfg:
    def __init__(self, enabled=True, provider="auto",
                 min_audio_size_kb=1.0, max_audio_size_kb=25000, local_model_size="base"):
        self.audio_enabled = enabled
        self.audio_provider = provider
        self.min_audio_size_kb = min_audio_size_kb
        self.max_audio_size_kb = max_audio_size_kb
        self.local_model_size = local_model_size


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
