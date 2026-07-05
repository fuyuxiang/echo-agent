from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any, Protocol

import aiohttp
from loguru import logger


class TranscribeBackend(Protocol):
    async def transcribe(self, path: Path) -> str: ...


def _cloud_available(api_key: str) -> bool:
    return bool(api_key)


def _local_available() -> bool:
    try:
        import faster_whisper  # noqa: F401
        return True
    except Exception:
        return False


class CloudWhisperBackend:
    """OpenAI-compatible /audio/transcriptions (Groq whisper-large-v3 by default)."""

    def __init__(
        self,
        api_key: str,
        *,
        model: str = "whisper-large-v3",
        base_url: str = "https://api.groq.com/openai/v1",
        timeout_sec: float = 60.0,
    ) -> None:
        self._api_key = api_key
        self._model = model
        self._base_url = base_url.rstrip("/")
        self._timeout_sec = timeout_sec

    async def transcribe(self, path: Path) -> str:
        if not self._api_key or not path.exists():
            return ""
        url = f"{self._base_url}/audio/transcriptions"
        try:
            with path.open("rb") as fh:
                data = aiohttp.FormData()
                data.add_field("file", fh, filename=path.name)
                data.add_field("model", self._model)
                timeout = aiohttp.ClientTimeout(total=self._timeout_sec)
                async with aiohttp.ClientSession(timeout=timeout) as session:
                    async with session.post(
                        url, data=data, headers={"Authorization": f"Bearer {self._api_key}"}
                    ) as resp:
                        if resp.status == 200:
                            result = await resp.json()
                            return (result.get("text") or "").strip()
                        logger.debug("cloud transcribe non-200 ({}): {}", resp.status, await resp.text())
        except Exception as e:  # fail-open
            logger.debug("cloud transcribe failed (fail-open): {}", e)
        return ""


class LocalWhisperBackend:
    """Local transcription via faster-whisper, run in a thread (blocking lib)."""

    def __init__(self, model_size: str = "base") -> None:
        self._model_size = model_size
        self._model: Any = None

    @staticmethod
    def _load_model_cls() -> Any:
        from faster_whisper import WhisperModel
        return WhisperModel

    def _transcribe_sync(self, path: Path) -> str:
        if self._model is None:
            model_cls = self._load_model_cls()
            self._model = model_cls(self._model_size)
        segments, _info = self._model.transcribe(str(path))
        return " ".join(seg.text.strip() for seg in segments if seg.text.strip())

    async def transcribe(self, path: Path) -> str:
        if not path.exists():
            return ""
        try:
            return await asyncio.to_thread(self._transcribe_sync, path)
        except Exception as e:  # fail-open
            logger.debug("local transcribe failed (fail-open): {}", e)
            return ""
