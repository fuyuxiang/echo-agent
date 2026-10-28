from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.agent.media.understanding.audio import (
    AudioTranscriber,
    CloudWhisperBackend,
    LocalWhisperBackend,
    _cloud_available,
    _local_available,
)
from echo_agent.agent.media.understanding.base import MediaUnderstanding
from echo_agent.agent.media.understanding.video import (
    LLMVisionBackend,
    VideoUnderstander,
    _ffmpeg_available,
)


def _select_backend(provider: str, api_key: str, model_size: str,
                    base_url: str, model: str) -> Any:
    """Return a TranscribeBackend per provider policy, or None if unavailable."""
    want_cloud = provider in ("auto", "cloud") and _cloud_available(api_key)
    want_local = provider in ("auto", "local") and _local_available()
    if provider == "cloud":
        return CloudWhisperBackend(api_key, base_url=base_url, model=model) if _cloud_available(api_key) else None
    if provider == "local":
        return LocalWhisperBackend(model_size) if _local_available() else None
    # auto: cloud first, then local
    if want_cloud:
        return CloudWhisperBackend(api_key, base_url=base_url, model=model)
    if want_local:
        return LocalWhisperBackend(model_size)
    return None


def default_understanders(config: Any, *, transcription_api_key: str = "",
                          vision_provider: Any = None) -> list[MediaUnderstanding]:
    """Assemble the ordered understander list from config + backend probing."""
    understanders: list[MediaUnderstanding] = []

    transcribe_backend = None
    if getattr(config, "audio_enabled", False):
        transcribe_backend = _select_backend(
            getattr(config, "audio_provider", "auto"),
            transcription_api_key,
            getattr(config, "local_model_size", "base"),
            getattr(config, "transcription_base_url", "https://api.groq.com/openai/v1"),
            getattr(config, "transcription_model", "whisper-large-v3"),
        )
        if transcribe_backend is not None:
            understanders.append(AudioTranscriber(
                transcribe_backend,
                min_size_kb=getattr(config, "min_audio_size_kb", 1.0),
                max_size_kb=getattr(config, "max_audio_size_kb", 25000),
            ))
        else:
            logger.debug("no transcribe backend available; audio understanding disabled")

    if getattr(config, "video_enabled", False) and _ffmpeg_available() and vision_provider is not None:
        vision = LLMVisionBackend(
            vision_provider,
            model=getattr(config, "video_vision_model", ""),
            prompt=getattr(config, "video_vision_prompt", "简要描述这段视频的画面内容。"),
        )
        understanders.append(VideoUnderstander(
            vision,
            transcribe_backend,  # reuse audio's backend for the video's audio track (may be None)
            frame_count=getattr(config, "video_frame_count", 4),
            min_size_kb=getattr(config, "min_video_size_kb", 1.0),
            max_size_kb=getattr(config, "max_video_size_kb", 204800),
            ffmpeg_concurrency=getattr(config, "video_ffmpeg_concurrency", 2),
        ))
    elif getattr(config, "video_enabled", False):
        logger.debug("video understanding unavailable (ffmpeg or vision provider missing)")

    return understanders
