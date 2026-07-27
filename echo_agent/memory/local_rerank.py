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
import threading
import time
from concurrent.futures import Future, ThreadPoolExecutor
from pathlib import Path
from typing import Any, Callable

from loguru import logger

_DEFAULT_MODEL = "BAAI/bge-reranker-base"

# Wall-clock ceiling for one mirror fetch attempt (~941 MiB across 10 volumes).
# A transfer that trickles below ~0.3 MB/s would otherwise never trip the
# per-read socket timeout and would keep a load thread — and the user's
# bandwidth — occupied indefinitely.
_DOWNLOAD_BUDGET_SECONDS = 3600.0


class _DownloadAborted(Exception):
    """Raised inside the load thread to stop a mirror fetch mid-stream."""


def _run_on_daemon_thread(fn: Callable[[], Any], name: str) -> Future[Any]:
    """Run *fn* on a daemon thread, exposing a Future the caller can await.

    Deliberately NOT a ThreadPoolExecutor submit: pool workers are non-daemon
    and ``concurrent.futures`` joins them in an atexit hook, so a model download
    still running at shutdown holds the interpreter open until it finishes —
    ``shutdown(wait=False, cancel_futures=True)`` only drops *queued* work, it
    cannot interrupt a thread already inside ``urlopen().read()``. A daemon
    thread lets the process exit; ``LocalReranker._closed`` is what stops the
    download promptly and cleanly.
    """
    fut: Future[Any] = Future()

    def runner() -> None:
        if not fut.set_running_or_notify_cancel():
            return
        try:
            fut.set_result(fn())
        except BaseException as e:  # noqa: BLE001 - propagated to the awaiter
            fut.set_exception(e)

    threading.Thread(target=runner, name=name, daemon=True).start()
    return fut

# Self-hosted release packages, tried before fastembed's HF download (CN-friendly,
# sha256-pinned). Unlike the embedding packages (flat GCS layout), the reranker
# has NO GCS `url` source in fastembed — its offline path is the HuggingFace hub
# cache layout `models--<repo>/{blobs,refs,snapshots}/`. So the tar's top-level
# dir MUST be that `models--…` directory, extracted straight into cache_dir; then
# fastembed's local_files_only load hits it. Verified offline before shipping.
#
# `sources` is tried in order and each entry is either a single-file tarball
# (`url`) or a set of ordered split volumes (`parts`) that concatenate back into
# the exact same tarball. Gitee caps a release asset at 100 MiB, and this tar is
# ~941 MiB, so the CN-friendly mirror can only host it as parts — which is why a
# parts source exists at all, and why it must be verified AFTER joining (the
# pinned sha256 is the whole tarball's, individual volumes have no digest).
# Gitee goes first: for CN networks GitHub release downloads are the flaky ones,
# and this is the same mirror order as the embedding package.
_RELEASE_PACKAGES: dict[str, dict[str, Any]] = {
    "BAAI/bge-reranker-base": {
        # The directory fastembed's HF cache expects inside cache_dir.
        "cache_subdir": "models--BAAI--bge-reranker-base",
        "sha256": "be3bcc7b24448b3467318f6b4e14fdf0f3e8d4ad0e3c2f1b612a1dd011163fd1",
        "sources": [
            {
                "parts": [
                    "https://gitee.com/fuyuxiang/echo-agent/releases/download/v0.3.6/bge-reranker-base-fastembed.tar.gz.part-%02d" % i
                    for i in range(10)
                ],
            },
            {
                "url": "https://github.com/fuyuxiang/echo-agent/releases/download/v0.3.6/bge-reranker-base-fastembed.tar.gz",
            },
        ],
    },
}


def _source_label(source: dict[str, Any]) -> str:
    """Human-readable name for a release source (a URL, or "N parts from <url0>")."""
    parts = source.get("parts")
    if parts:
        return f"{len(parts)} parts from {parts[0]}"
    return str(source.get("url", "<unknown>"))


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
        # Dedicated pool so reranker inference never starves the shared executor
        # (sessions, provider streaming, embedding load). Inference only — the
        # load/download runs on its own daemon thread (see _run_on_daemon_thread)
        # so it neither occupies this single worker for the length of a ~941MB
        # download nor keeps the interpreter alive at exit.
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
        half-tree that a later cache-hit check would trust.

        A source is either one tarball or ordered split volumes joined back into
        it; either way the pinned sha256 is checked on the assembled file, so a
        truncated or out-of-order join is rejected exactly like a corrupt
        download.

        The wall-clock budget is computed once here and shared by every source, so
        the fallback mirror inherits whatever is left instead of getting a fresh
        hour of its own (which would make one load() take up to
        len(sources) * _DOWNLOAD_BUDGET_SECONDS)."""
        pkg = _RELEASE_PACKAGES.get(self._model_name)
        if pkg is None or not self._cache_dir:
            return False
        import shutil
        import tarfile
        import tempfile

        cache_root = Path(self._cache_dir).resolve()
        target = cache_root / pkg["cache_subdir"]
        if _hf_cache_has_ready_model(target):
            return True
        os.makedirs(self._cache_dir, exist_ok=True)
        deadline = time.monotonic() + _DOWNLOAD_BUDGET_SECONDS
        for source in pkg["sources"]:
            if self._closed or time.monotonic() >= deadline:
                # close() or budget exhausted during the previous source: don't
                # start another ~941MB transfer just because a mirror failed.
                return False
            tmp = None
            staging = None
            try:
                tmp, digest = self._download_source(source, deadline)
                if tmp is None:
                    continue
                # The stages below (hash compare, ~941MB gzip extract, tree
                # rename) run for minutes on slow disks, so they honour close()
                # too — otherwise shutdown would block on the extract.
                if self._closed:
                    logger.debug("Reranker release install aborted: reranker closed")
                    return False
                if digest != pkg["sha256"]:
                    logger.warning(
                        "Reranker release sha256 mismatch from {}", _source_label(source)
                    )
                    continue
                staging = tempfile.mkdtemp(dir=self._cache_dir, prefix=".staging-")
                staging_root = Path(staging).resolve()
                with tarfile.open(tmp, "r:gz") as tar:
                    # Path-traversal guard without the 3.12+ filter= arg (floor is
                    # 3.11): every member must resolve inside the staging root.
                    members = tar.getmembers()
                    for member in members:
                        dest = (staging_root / member.name).resolve()
                        if dest != staging_root and staging_root not in dest.parents:
                            raise ValueError(f"unsafe tar member: {member.name}")
                    # Extract member-by-member instead of extractall() so close()
                    # interrupts a multi-minute unpack; the staging dir is removed
                    # in `finally`, so a partial tree is never left behind.
                    for member in members:
                        if self._closed:
                            raise _DownloadAborted("reranker closed")
                        tar.extract(member, staging)
                extracted = staging_root / pkg["cache_subdir"]
                if not _hf_cache_has_ready_model(extracted):
                    logger.warning(
                        "Reranker release incomplete after extract from {}",
                        _source_label(source),
                    )
                    continue
                if self._closed:
                    # Never touch the real cache dir mid-shutdown: the rmtree +
                    # os.replace pair below is not atomic as a whole.
                    logger.debug("Reranker release install aborted: reranker closed")
                    return False
                if target.exists():
                    shutil.rmtree(target, ignore_errors=True)
                os.replace(extracted, target)
                logger.info(
                    "Reranker model fetched from release mirror: {}", _source_label(source)
                )
                return True
            except _DownloadAborted as e:
                # Normal shutdown, not a broken mirror: no warning, no next source.
                logger.debug("Reranker release install aborted: {}", e)
                return False
            except Exception as e:
                logger.warning(
                    "Reranker release fetch failed from {}: {}", _source_label(source), e
                )
            finally:
                if tmp:
                    Path(tmp).unlink(missing_ok=True)
                if staging:
                    shutil.rmtree(staging, ignore_errors=True)
        return False

    def _download_source(
        self, source: dict[str, Any], deadline: float | None = None
    ) -> tuple[str | None, str]:
        """Download one source to a temp file; returns (path, sha256) or (None, "").

        Volumes are streamed straight into a single file in order and hashed as
        they land, so a ~1GB package never needs to be held in memory or stored
        twice on disk. Any missing volume aborts the whole source — a partial
        join would only fail the digest check later, so failing here keeps the
        warning pointed at the URL that actually broke.

        The chunk loop checks ``self._closed`` and the wall-clock budget between
        chunks, so `close()` (shutdown, or the operator turning reranking off)
        stops a ~941MB transfer within one chunk instead of running it to
        completion. `deadline` is the *shared* budget across all sources of one
        fetch; it defaults to a fresh budget only for direct callers/tests.
        """
        import hashlib
        import tempfile
        import urllib.request

        urls = source.get("parts") or [source["url"]]
        hasher = hashlib.sha256()
        tmp = None
        if deadline is None:
            deadline = time.monotonic() + _DOWNLOAD_BUDGET_SECONDS
        try:
            with tempfile.NamedTemporaryFile(dir=self._cache_dir, delete=False) as f:
                tmp = f.name
                for url in urls:
                    self._check_download_allowed(deadline)
                    with urllib.request.urlopen(url, timeout=30) as resp:
                        while True:
                            chunk = resp.read(1024 * 1024)
                            if not chunk:
                                break
                            hasher.update(chunk)
                            f.write(chunk)
                            self._check_download_allowed(deadline)
            return tmp, hasher.hexdigest()
        except _DownloadAborted as e:
            # Not a failure of the mirror: log at debug and let the caller move on
            # without a scary warning during a normal shutdown.
            logger.debug("Reranker release download aborted: {}", e)
            if tmp:
                Path(tmp).unlink(missing_ok=True)
            return None, ""
        except Exception as e:
            logger.warning("Reranker release download failed: {}", e)
            if tmp:
                Path(tmp).unlink(missing_ok=True)
            return None, ""

    def _check_download_allowed(self, deadline: float) -> None:
        """Raise _DownloadAborted when the fetch should stop mid-stream."""
        if self._closed:
            raise _DownloadAborted("reranker closed")
        if time.monotonic() >= deadline:
            raise _DownloadAborted(
                f"exceeded the {_DOWNLOAD_BUDGET_SECONDS:.0f}s download budget"
            )

    def _load_model_sync(self) -> Any | None:
        try:
            # Our own release mirror first (CN-friendly, sha256-pinned). Success
            # lets us pin local_files_only so fastembed does NOT probe HF online
            # first (which on CN networks can 401 via Xet and strand the ready
            # local cache). Failure falls through to fastembed's own HF download.
            ready = self._fetch_release_package()
            if self._closed:
                # Closed while the mirror fetch was streaming: don't go on to
                # instantiate a ~1GB ONNX session nobody will use.
                return None
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
                fut = _run_on_daemon_thread(
                    self._load_model_sync, "local-rerank-load"
                )
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
        """Stop any in-flight download and release the inference pool.

        Idempotent and non-blocking. Setting ``_closed`` first is what actually
        cancels a running mirror fetch: it is checked between download chunks
        (see ``_check_download_allowed``), between tar members while unpacking,
        and before the cache dir is replaced — so no stage of a ~941MB install
        runs to completion after shutdown starts. The load thread is a daemon, so
        even a fetch parked inside a socket read cannot delay interpreter exit.
        """
        self._closed = True
        self._load_future = None
        pool, self._pool = self._pool, None
        if pool is not None:
            pool.shutdown(wait=False, cancel_futures=True)
