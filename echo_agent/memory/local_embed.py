"""Local embedding fallback — fastembed (ONNX, CPU) so vector search works
with zero configuration when no embed-capable provider is registered.

Lazy loading: the ONNX model is NOT loaded at construction (first use may
download ~100MB). Load and inference both run in an executor thread to keep
the async embed_fn contract without blocking the event loop.
"""

from __future__ import annotations

import asyncio
import importlib
from typing import Any

from loguru import logger

# Known model dimensions so callers (knowledge attach, VectorIndex) can size
# themselves before the model is actually loaded.
_KNOWN_DIMS: dict[str, int] = {
    "BAAI/bge-small-zh-v1.5": 512,
    "BAAI/bge-small-en-v1.5": 384,
    "jinaai/jina-embeddings-v2-base-zh": 768,
}

_DEFAULT_MODEL = "BAAI/bge-small-zh-v1.5"


class LocalEmbedder:
    """fastembed-backed local embedder with lazy load and graceful failure."""

    def __init__(self, model_name: str = _DEFAULT_MODEL):
        self._model_name = model_name
        self._model: Any | None = None
        self._load_failed = False
        self._load_lock = asyncio.Lock()

    @property
    def available(self) -> bool:
        try:
            return importlib.import_module("fastembed") is not None
        except ImportError:
            return False

    @property
    def model_id(self) -> str:
        return f"fastembed:{self._model_name}"

    @property
    def dimensions(self) -> int:
        return _KNOWN_DIMS.get(self._model_name, 0)

    def _load_model_sync(self) -> Any | None:
        try:
            fastembed = importlib.import_module("fastembed")
            return fastembed.TextEmbedding(model_name=self._model_name)
        except Exception as e:
            logger.warning(
                "Local embedding model '{}' failed to load (offline or download "
                "failed?); memory retrieval degrades to keyword-only: {}",
                self._model_name, e,
            )
            return None

    def _embed_sync(self, text: str) -> list[float] | None:
        assert self._model is not None
        vectors = list(self._model.embed([text]))
        if not vectors:
            return None
        return [float(x) for x in vectors[0]]

    async def embed(self, text: str) -> list[float] | None:
        """Embed *text*; returns None on any failure (caller degrades)."""
        if self._load_failed or not self.available:
            return None
        loop = asyncio.get_running_loop()
        if self._model is None:
            async with self._load_lock:
                if self._model is None and not self._load_failed:
                    model = await loop.run_in_executor(None, self._load_model_sync)
                    if model is None:
                        # Do not retry the download on every message — one
                        # failure marks the embedder dead for this process.
                        self._load_failed = True
                        return None
                    self._model = model
        try:
            return await loop.run_in_executor(None, self._embed_sync, text)
        except Exception as e:
            logger.debug("Local embedding failed: {}", e)
            return None
