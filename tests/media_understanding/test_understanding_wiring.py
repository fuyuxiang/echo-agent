import inspect

from echo_agent.agent import loop as loop_mod


def test_loop_wires_understanders_into_context_builder():
    src = inspect.getsource(loop_mod)
    # ContextBuilder is constructed with understanders assembled from config
    assert "default_understanders" in src
    assert "understanders=" in src


def test_transcribe_audio_delegates_to_backend():
    from echo_agent.channels.base import BaseChannel
    src = inspect.getsource(BaseChannel.transcribe_audio)
    # dead Groq inline impl replaced by delegation to the understanding backend
    assert "CloudWhisperBackend" in src
