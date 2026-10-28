from echo_agent.config.schema import Config, MediaUnderstandingConfig


def test_defaults():
    c = MediaUnderstandingConfig()
    assert c.audio_enabled is True
    assert c.audio_provider == "auto"
    assert c.min_audio_size_kb == 1.0
    assert c.max_audio_size_kb == 25000
    assert c.local_model_size == "base"
    assert c.transcription_base_url == "https://api.groq.com/openai/v1"
    assert c.transcription_model == "whisper-large-v3"
    assert c.video_enabled is True
    assert c.video_frame_count == 4
    assert c.video_vision_model == ""
    assert c.video_vision_prompt == "简要描述这段视频的画面内容。"
    assert c.min_video_size_kb == 1.0
    assert c.max_video_size_kb == 204800


def test_mounted_on_config():
    c = Config()
    assert isinstance(c.media_understanding, MediaUnderstandingConfig)
    assert c.media_understanding.audio_enabled is True
