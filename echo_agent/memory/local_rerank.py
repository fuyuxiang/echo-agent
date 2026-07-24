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
from pathlib import Path
from typing import Any

from loguru import logger

_DEFAULT_MODEL = "BAAI/bge-reranker-base"

# Self-hosted release packages, tried before fastembed's HF download (CN-friendly,
# sha256-pinned). Unlike the embedding packages (flat GCS layout), the reranker
# has NO GCS `url` source in fastembed — its offline path is the HuggingFace hub
# cache layout `models--<repo>/{blobs,refs,snapshots}/`. So the tar's top-level
# dir MUST be that `models--…` directory, extracted straight into cache_dir; then
# fastembed's local_files_only load hits it. Verified offline before shipping.
_RELEASE_PACKAGES: dict[str, dict[str, Any]] = {
    "BAAI/bge-reranker-base": {
        # The directory fastembed's HF cache expects inside cache_dir.
        "cache_subdir": "models--BAAI--bge-reranker-base",
        "sha256": "be3bcc7b24448b3467318f6b4e14fdf0f3e8d4ad0e3c2f1b612a1dd011163fd1",
        "urls": [
            "https://github.com/fuyuxiang/echo-agent/releases/download/v0.3.6/bge-reranker-base-fastembed.tar.gz",
        ],
    },
}


def _hf_cache_has_ready_model(cache_subdir: Path) -> bool:
    """True when a HF-layout cache dir holds a ready ONNX model.

    Mirrors the embedder's readiness check but for HF hub layout: look under
    snapshots/*/onnx/model.onnx and require it to resolve to a non-empty file
    (following the blob symlink). A half-extracted / empty tree is not a hit, so
    an interrupted fetch re-downloads instead of being trusted."""
    try:
        for onnx in cache_subdir.glob("snapshots/*/onnx/model.onnx"):
            # stat() follows symlinks → catches a dangling link or empty blob.
            if onnx.is_file() and onnx.stat().st_size > 0:
                return True
    except OSError:
        return False
    return False


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

    def _fetch_release_package(self) -> bool:
        """Populate the fastembed HF cache from our own release mirror.

        Runs inside the load thread (never on the event loop). Returns True when
        the cache now holds the model (so the caller pins local_files_only); False
        falls through to fastembed's own HF download. Best-effort by design. The
        tar's top-level dir is the HF-layout `models--…` directory, extracted
        straight into cache_dir. Download → sha256 verify → extract to a same-disk
        staging dir → atomic rename, so an interrupted fetch never leaves a
        half-tree that a later cache-hit check would trust."""
        pkg = _RELEASE_PACKAGES.get(self._model_name)
        if pkg is None or not self._cache_dir:
            return False
        import hashlib
        import shutil
        import tarfile
        import tempfile
        import urllib.request

        cache_root = Path(self._cache_dir).resolve()
        target = cache_root / pkg["cache_subdir"]
        if _hf_cache_has_ready_model(target):
            return True
        os.makedirs(self._cache_dir, exist_ok=True)
        for url in pkg["urls"]:
            tmp = None
            staging = None
            try:
                with urllib.request.urlopen(url, timeout=30) as resp, \
                        tempfile.NamedTemporaryFile(dir=self._cache_dir, delete=False) as f:
                    tmp = f.name
                    shutil.copyfileobj(resp, f)
                digest = hashlib.sha256(Path(tmp).read_bytes()).hexdigest()
                if digest != pkg["sha256"]:
                    logger.warning("Reranker release sha256 mismatch from {}", url)
                    continue
                staging = tempfile.mkdtemp(dir=self._cache_dir, prefix=".staging-")
                staging_root = Path(staging).resolve()
                with tarfile.open(tmp, "r:gz") as tar:
                    # Path-traversal guard without the 3.12+ filter= arg (floor is
                    # 3.11): every member must resolve inside the staging root.
                    for member in tar.getmembers():
                        dest = (staging_root / member.name).resolve()
                        if dest != staging_root and staging_root not in dest.parents:
                            raise ValueError(f"unsafe tar member: {member.name}")
                    tar.extractall(staging)
                extracted = staging_root / pkg["cache_subdir"]
                if not _hf_cache_has_ready_model(extracted):
                    logger.warning("Reranker release incomplete after extract from {}", url)
                    continue
                if target.exists():
                    shutil.rmtree(target, ignore_errors=True)
                os.replace(extracted, target)
                logger.info("Reranker model fetched from release mirror: {}", url)
                return True
            except Exception as e:
                logger.warning("Reranker release fetch failed from {}: {}", url, e)
            finally:
                if tmp:
                    Path(tmp).unlink(missing_ok=True)
                if staging:
                    shutil.rmtree(staging, ignore_errors=True)
        return False

    def _load_model_sync(self) -> Any | None:
        try:
            # Our own release mirror first (CN-friendly, sha256-pinned). Success
            # lets us pin local_files_only so fastembed does NOT probe HF online
            # first (which on CN networks can 401 via Xet and strand the ready
            # local cache). Failure falls through to fastembed's own HF download.
            ready = self._fetch_release_package()
            os.environ.setdefault("HF_HUB_DOWNLOAD_TIMEOUT", "15")
            os.environ.setdefault("HF_HUB_ETAG_TIMEOUT", "15")
            if self._hf_endpoint:
                os.environ.setdefault("HF_ENDPOINT", self._hf_endpoint)
            mod = importlib.import_module("fastembed.rerank.cross_encoder")
            kwargs: dict[str, Any] = {"model_name": self._model_name}
            if self._cache_dir:
                kwargs["cache_dir"] = self._cache_dir
            if ready:
                kwargs["local_files_only"] = True
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
