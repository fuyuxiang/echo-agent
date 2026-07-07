"""Local embedding fallback — fastembed (ONNX, CPU) so vector search works
with zero configuration when no embed-capable provider is registered.

Lazy loading: the ONNX model is NOT loaded at construction (first use may
download ~100MB). Load and inference both run in an executor thread to keep
the async embed_fn contract without blocking the event loop.
"""

from __future__ import annotations

import asyncio
import importlib
import importlib.util
import os
import sys
from concurrent.futures import ThreadPoolExecutor
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

    def __init__(self, model_name: str = _DEFAULT_MODEL, load_timeout_seconds: float = 60.0):
        self._model_name = model_name
        self._model: Any | None = None
        self._load_failed = False
        self._load_lock = asyncio.Lock()
        self._load_timeout = load_timeout_seconds
        # Dedicated single-thread pool so a hung model download never occupies a
        # worker in the shared default executor (which also serves session IO,
        # provider streaming, etc.). A stuck download stays isolated here.
        self._pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="local-embed")

    @property
    def available(self) -> bool:
        # If fastembed is already imported, it's obviously available. Otherwise
        # probe with find_spec, which locates the module WITHOUT importing it
        # (and its heavy onnxruntime/tokenizers deps), so calling this on the
        # loop thread at startup no longer stalls the loop.
        if sys.modules.get("fastembed") is not None:
            return True
        try:
            return importlib.util.find_spec("fastembed") is not None
        except (ImportError, ValueError):
            return False

    @property
    def model_id(self) -> str:
        return f"fastembed:{self._model_name}"

    @property
    def dimensions(self) -> int:
        return _KNOWN_DIMS.get(self._model_name, 0)

    def _load_model_sync(self) -> Any | None:
        try:
            # Bound the underlying HF socket so a slow/dead mirror fails fast at
            # the transport layer instead of relying solely on the outer
            # wait_for (which cannot interrupt a blocked C-level socket read).
            os.environ.setdefault("HF_HUB_DOWNLOAD_TIMEOUT", "30")
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
                    try:
                        # wait_for bounds the load: a hung network download would
                        # otherwise hold the lock forever and never set
                        # _load_failed, leaving the "download hangs" failure mode
                        # uncovered (only exceptions were handled before).
                        model = await asyncio.wait_for(
                            loop.run_in_executor(self._pool, self._load_model_sync),
                            timeout=self._load_timeout,
                        )
                    except (asyncio.TimeoutError, TimeoutError):
                        # The executor thread may still be stuck downloading, but
                        # it is isolated in our dedicated single-thread pool, so
                        # it cannot starve the shared default executor. Mark the
                        # embedder dead so we never wait on it again this process.
                        self._load_failed = True
                        logger.warning(
                            "Local embedding model '{}' load timed out after {}s; "
                            "memory retrieval degrades to keyword-only for this process",
                            self._model_name, self._load_timeout,
                        )
                        return None
                    if model is None:
                        # Do not retry the download on every message — one
                        # failure marks the embedder dead for this process.
                        self._load_failed = True
                        return None
                    self._model = model
        try:
            return await loop.run_in_executor(self._pool, self._embed_sync, text)
        except Exception as e:
            logger.debug("Local embedding failed: {}", e)
            return None

    def close(self) -> None:
        """Release the dedicated thread pool.

        Idempotent and safe to call during shutdown. A thread still stuck in a
        hung model download cannot be force-killed, so we do NOT wait for it
        (wait=False) — otherwise stop() could block for the life of that
        download. cancel_futures drops any not-yet-started work. Marking the
        embedder failed makes any late embed() call return None instead of
        submitting to a pool that is (or is about to be) shut down."""
        self._load_failed = True
        pool, self._pool = self._pool, None
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)
