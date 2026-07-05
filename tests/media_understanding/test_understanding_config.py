from echo_agent.config.schema import Config, MediaUnderstandingConfig


def test_defaults():
    c = MediaUnderstandingConfig()
    assert c.audio_enabled is True
    assert c.audio_provider == "auto"
    assert c.min_audio_size_kb == 1.0
    assert c.max_audio_size_kb == 25000
    assert c.local_model_size == "base"


def test_mounted_on_config():
    c = Config()
    assert isinstance(c.media_understanding, MediaUnderstandingConfig)
    assert c.media_understanding.audio_enabled is True
