"""Local embedding fallback — fastembed (ONNX, CPU) so vector search works
with zero configuration when no embed-capable provider is registered.

Lazy loading: the ONNX model is NOT loaded at construction (first use may
download ~50MB). Load and inference both run in an executor thread to keep
the async embed_fn contract without blocking the event loop.

Load/wait decoupling: a model download can take far longer than any single
message's latency budget (a ~50MB model over a slow mirror is minutes, not
seconds). So the download runs as ONE background future; each embed() call
merely *waits* on that future for a short per-call budget. A budget timeout
degrades only that turn to keyword-only — the download keeps running in the
background and the model is picked up transparently on a later call. This is
what stops a nearly-complete download from being thrown away and restarted on
every message, and stops one slow load from wedging the process.

Backoff on failure: a genuine load failure (not a slow one) is retried with
growing backoff rather than permanently disabling the embedder, so a transient
network outage does not doom the whole process to keyword-only until restart.
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

# Known model dimensions so callers (knowledge attach, VectorIndex) can size
# themselves before the model is actually loaded.
_KNOWN_DIMS: dict[str, int] = {
    "BAAI/bge-small-zh-v1.5": 512,
    "BAAI/bge-small-en-v1.5": 384,
    "jinaai/jina-embeddings-v2-base-zh": 768,
}

_DEFAULT_MODEL = "BAAI/bge-small-zh-v1.5"

# Self-hosted release packages, tried before fastembed's HF/GCS download.
# Key = model name; the tar's top-level dir must equal cache_subdir (that is
# fastembed's cache-hit layout, verified offline).
_RELEASE_PACKAGES: dict[str, dict[str, Any]] = {
    "BAAI/bge-small-zh-v1.5": {
        "cache_subdir": "fast-bge-small-zh-v1.5",
        "sha256": "d095c530b22f384d4d19a79c5862b65e8fff104af64ce9bb9e89690c186d418f",
        "urls": [
            "https://gitee.com/fuyuxiang/echo-agent/releases/download/v1.1.0/bge-small-zh-v1.5-fastembed.tar.gz",
            "https://github.com/fuyuxiang/echo-agent/releases/download/V1.1.0/bge-small-zh-v1.5-fastembed.tar.gz",
        ],
    },
}


class LocalEmbedder:
    """fastembed-backed local embedder with lazy load and graceful failure."""

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
        # Per-call budget: how long a single embed() waits on the (possibly
        # still-running) background load before degrading THIS turn to
        # keyword-only. The load itself is not bounded by this — it keeps going.
        self._load_timeout = load_timeout_seconds
        # HuggingFace download mirror (e.g. https://hf-mirror.com for CN
        # networks). Empty string leaves any existing HF_ENDPOINT untouched.
        self._hf_endpoint = hf_endpoint
        # Explicit fastembed cache directory. When set, install-time prefetch
        # and runtime resolve the SAME location, so a preloaded model is an
        # offline cache hit at runtime. Empty leaves fastembed's default
        # (FASTEMBED_CACHE_PATH env or a volatile tempdir) untouched.
        self._cache_dir = cache_dir
        # Bounded backoff retry: a failed load (network blip, dead mirror) is
        # retried up to _max_load_attempts times, waiting _retry_backoff before
        # re-arming, instead of disabling the embedder for the whole process.
        self._max_load_attempts = max(1, int(max_load_attempts))
        self._retry_backoff = max(0.0, float(retry_backoff_seconds))
        self._load_attempts = 0
        self._next_retry_at = 0.0
        # The single in-flight background load. Reused across concurrent embed()
        # calls so a slow download is started once and merely awaited, never
        # duplicated or restarted while running.
        self._load_future: Future[Any] | None = None
        self._load_lock = asyncio.Lock()
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

    def _fetch_release_package(self) -> bool:
        """Populate the fastembed cache from our own release mirrors.

        Runs inside the load thread (never on the event loop). Returns True when
        the cache dir now holds the model; False falls through to fastembed's own
        HF/GCS download. Best-effort by design."""
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
        if (target / "model_optimized.onnx").exists():
            return True
        os.makedirs(self._cache_dir, exist_ok=True)
        for url in pkg["urls"]:
            tmp = None
            try:
                with urllib.request.urlopen(url, timeout=30) as resp, \
                        tempfile.NamedTemporaryFile(dir=self._cache_dir, delete=False) as f:
                    tmp = f.name
                    shutil.copyfileobj(resp, f)
                digest = hashlib.sha256(Path(tmp).read_bytes()).hexdigest()
                if digest != pkg["sha256"]:
                    logger.warning("Release model sha256 mismatch from {}", url)
                    continue
                with tarfile.open(tmp, "r:gz") as tar:
                    # Guard against path traversal without relying on the 3.12+
                    # `filter="data"` arg (project floor is Python 3.11): every
                    # member must resolve to a path inside the cache root.
                    for member in tar.getmembers():
                        dest = (cache_root / member.name).resolve()
                        if dest != cache_root and cache_root not in dest.parents:
                            raise ValueError(f"unsafe tar member: {member.name}")
                    tar.extractall(self._cache_dir)
                if (target / "model_optimized.onnx").exists():
                    logger.info("Embedding model fetched from release mirror: {}", url)
                    return True
            except Exception as e:
                logger.warning("Release model fetch failed from {}: {}", url, e)
            finally:
                if tmp:
                    Path(tmp).unlink(missing_ok=True)
        return False

    def _load_model_sync(self) -> Any | None:
        try:
            # Our own release mirrors first (CN-friendly, sha256-pinned).
            # Failure silently falls through to the HF/GCS path below.
            self._fetch_release_package()
            # Bound the underlying HF socket so a slow/dead mirror fails fast at
            # the transport layer. HF_HUB_DOWNLOAD_TIMEOUT covers file transfer;
            # HF_HUB_ETAG_TIMEOUT covers the metadata calls (model_info /
            # list_repo_tree) that actually block first — without it a dead
            # mirror can stall for minutes on metadata before fastembed ever
            # falls back to the GCS url source. Keeping both tight is what lets
            # the GCS fallback be reached quickly on CN networks.
            os.environ.setdefault("HF_HUB_DOWNLOAD_TIMEOUT", "15")
            os.environ.setdefault("HF_HUB_ETAG_TIMEOUT", "15")
            # Route the model download through the configured mirror. setdefault
            # so an operator-provided HF_ENDPOINT env var always wins.
            if self._hf_endpoint:
                os.environ.setdefault("HF_ENDPOINT", self._hf_endpoint)
            fastembed = importlib.import_module("fastembed")
            kwargs: dict[str, Any] = {"model_name": self._model_name}
            if self._cache_dir:
                kwargs["cache_dir"] = self._cache_dir
            return fastembed.TextEmbedding(**kwargs)
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

    async def _ensure_model(self) -> Any | None:
        """Return the loaded model, or None if it isn't ready for this turn.

        Starts a single background load future (shared across concurrent calls)
        and waits on it for at most the per-call budget. On budget timeout the
        download keeps running in the background — a later call re-attaches to
        the same future and picks up the model when it lands. A genuine load
        failure arms bounded backoff instead of disabling the embedder forever.
        """
        async with self._load_lock:
            if self._model is not None:
                return self._model
            if self._closed or self._pool is None:
                return None
            fut = self._load_future
            if fut is None:
                if self._load_attempts >= self._max_load_attempts:
                    return None  # exhausted retries; keyword-only until restart
                if time.monotonic() < self._next_retry_at:
                    return None  # still within backoff window after a failure
                self._load_attempts += 1
                fut = self._pool.submit(self._load_model_sync)
                self._load_future = fut

        try:
            # wait_for cancels only the throwaway wrapper on timeout; a thread
            # already running the download cannot be cancelled, so the download
            # continues and the SAME concurrent future is awaited again next call.
            model = await asyncio.wait_for(
                asyncio.wrap_future(fut), timeout=self._load_timeout,
            )
        except (asyncio.TimeoutError, TimeoutError):
            logger.debug(
                "Local embedding model '{}' still loading after {}s; this turn "
                "degrades to keyword-only (download continues in background)",
                self._model_name, self._load_timeout,
            )
            return None
        except Exception:  # pragma: no cover - _load_model_sync already guards
            model = None

        async with self._load_lock:
            if self._load_future is fut:
                # First waiter to observe completion clears the slot and records
                # the outcome; a failure arms backoff for the next attempt.
                self._load_future = None
                if model is None:
                    self._next_retry_at = time.monotonic() + self._retry_backoff
                    if self._load_attempts >= self._max_load_attempts:
                        logger.warning(
                            "Local embedding model '{}' failed to load after {} "
                            "attempts; memory retrieval stays keyword-only until "
                            "restart",
                            self._model_name, self._load_attempts,
                        )
            if model is not None and self._model is None:
                self._model = model
        return model

    async def embed(self, text: str) -> list[float] | None:
        """Embed *text*; returns None on any failure (caller degrades)."""
        if self._closed or not self.available:
            return None
        if self._model is None and await self._ensure_model() is None:
            return None
        loop = asyncio.get_running_loop()
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
        embedder closed makes any late embed() call return None instead of
        submitting to a pool that is (or is about to be) shut down."""
        self._closed = True
        self._load_future = None
        pool, self._pool = self._pool, None
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)
