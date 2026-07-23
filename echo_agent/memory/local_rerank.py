"""Local cross-encoder reranker — fastembed TextCrossEncoder (ONNX, CPU).

The hybrid retriever fuses BM25 + vector rankings with RRF, which is purely
rank-based: it fuses the ORDER of two signals but has no notion of how relevant
a candidate actually is to the query. A cross-encoder scores each (query, doc)
pair jointly and is the precision gold standard for "is this actually relevant".
Running it over the small fused top-K (not the whole store) is cheap and turns
"best of whatever ranked" into "best of what's genuinely on-topic".

Same operational contract as LocalEmbedder: lazy load (first use may download
the model), load + inference both on a dedicated single-thread pool so the event
loop never blocks, a per-call wait budget that degrades THIS turn (keep the RRF
order) on timeout while the download continues, and bounded backoff on genuine
load failure. rerank() returns None on any failure so the caller degrades to the
un-reranked order rather than dropping recall.
"""

from __future__ import annotations

import asyncio
import importlib
import importlib.util
import os
import sys
import time
from concurrent.futures import Future, ThreadPoolExecutor
from typing import Any

from loguru import logger

_DEFAULT_MODEL = "BAAI/bge-reranker-base"


class LocalReranker:
    """fastembed TextCrossEncoder with lazy load and graceful failure."""

    def __init__(
        self,
        model_name: str = _DEFAULT_MODEL,
        load_timeout_seconds: float = 60.0,
        hf_endpoint: str = "",
        cache_dir: str = "",
        max_load_attempts: int = 5,
        retry_backoff_seconds: float = 30.0,
    ):
        self._model_name = model_name
        self._model: Any | None = None
        self._closed = False
        self._load_timeout = load_timeout_seconds
        self._hf_endpoint = hf_endpoint
        self._cache_dir = cache_dir
        self._max_load_attempts = max(1, int(max_load_attempts))
        self._retry_backoff = max(0.0, float(retry_backoff_seconds))
        self._load_attempts = 0
        self._next_retry_at = 0.0
        self._load_future: Future[Any] | None = None
        self._load_lock = asyncio.Lock()
        # Dedicated pool so a hung reranker download never starves the shared
        # executor (sessions, provider streaming, embedding load).
        self._pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="local-rerank")

    @property
    def available(self) -> bool:
        # find_spec locates fastembed WITHOUT importing its heavy deps, so this
        # is safe to probe on the loop thread at startup.
        if sys.modules.get("fastembed") is not None:
            return True
        try:
            return importlib.util.find_spec("fastembed") is not None
        except (ImportError, ValueError):
            return False

    @property
    def model_id(self) -> str:
        return f"fastembed-rerank:{self._model_name}"

    def _load_model_sync(self) -> Any | None:
        try:
            os.environ.setdefault("HF_HUB_DOWNLOAD_TIMEOUT", "15")
            os.environ.setdefault("HF_HUB_ETAG_TIMEOUT", "15")
            if self._hf_endpoint:
                os.environ.setdefault("HF_ENDPOINT", self._hf_endpoint)
            mod = importlib.import_module("fastembed.rerank.cross_encoder")
            kwargs: dict[str, Any] = {"model_name": self._model_name}
            if self._cache_dir:
                kwargs["cache_dir"] = self._cache_dir
            return mod.TextCrossEncoder(**kwargs)
        except Exception as e:
            logger.warning(
                "Local reranker '{}' failed to load (offline or download failed?); "
                "retrieval keeps the un-reranked RRF order: {}",
                self._model_name, e,
            )
            return None

    def _rerank_sync(self, query: str, documents: list[str]) -> list[float] | None:
        assert self._model is not None
        scores = list(self._model.rerank(query, documents))
        return [float(s) for s in scores]

    async def _ensure_model(self) -> Any | None:
        """Return the loaded model, or None if it isn't ready this turn.

        One shared background load future; each call waits at most the per-call
        budget. Timeout keeps the download running and degrades this turn. A
        genuine failure arms bounded backoff instead of disabling forever.
        """
        async with self._load_lock:
            if self._model is not None:
                return self._model
            if self._closed or self._pool is None:
                return None
            fut = self._load_future
            if fut is None:
                if self._load_attempts >= self._max_load_attempts:
                    return None
                if time.monotonic() < self._next_retry_at:
                    return None
                self._load_attempts += 1
                fut = self._pool.submit(self._load_model_sync)
                self._load_future = fut

        try:
            model = await asyncio.wait_for(
                asyncio.wrap_future(fut), timeout=self._load_timeout,
            )
        except (asyncio.TimeoutError, TimeoutError):
            logger.debug(
                "Local reranker '{}' still loading after {}s; this turn keeps the "
                "RRF order (download continues in background)",
                self._model_name, self._load_timeout,
            )
            return None
        except Exception:  # pragma: no cover - _load_model_sync guards
            model = None

        async with self._load_lock:
            if self._load_future is fut:
                self._load_future = None
                if model is None:
                    self._next_retry_at = time.monotonic() + self._retry_backoff
                    if self._load_attempts >= self._max_load_attempts:
                        logger.warning(
                            "Local reranker '{}' failed to load after {} attempts; "
                            "retrieval stays un-reranked until restart",
                            self._model_name, self._load_attempts,
                        )
            if model is not None and self._model is None:
                self._model = model
        return model

    async def rerank(self, query: str, documents: list[str]) -> list[float] | None:
        """Score each doc against *query*; returns aligned scores or None on failure."""
        if self._closed or not self.available or not documents:
            return None
        if self._model is None and await self._ensure_model() is None:
            return None
        loop = asyncio.get_running_loop()
        try:
            return await loop.run_in_executor(
                self._pool, self._rerank_sync, query, list(documents)
            )
        except Exception as e:
            logger.debug("Local rerank failed: {}", e)
            return None

    def close(self) -> None:
        """Release the dedicated pool (idempotent, non-blocking on hung loads)."""
        self._closed = True
        self._load_future = None
        pool, self._pool = self._pool, None
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)
