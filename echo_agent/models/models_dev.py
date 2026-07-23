"""models.dev integration — live per-model context-window database.

models.dev (https://models.dev/api.json) is a community-maintained catalog of
4000+ models across 100+ providers. Each model carries a ``limit.context`` —
its real context window. We use it as a dynamic resolution layer so a model
that ships after this build still resolves to its true window without a code
change, instead of falling back to a hardcoded default.

Design constraints (measured: the api.json fetch is ~3MB / ~14s from CN):
  * ``lookup_context`` is a hot-path helper called from the (synchronous)
    window resolver — it MUST NOT block on the network. It only ever reads the
    in-memory / on-disk cache and returns 0 on a miss.
  * The network fetch runs fire-and-forget in a daemon thread, triggered when
    the cache is empty or stale. The first caller on a cold, cache-less install
    gets 0 (the built-in registry covers that round); the refresh fills the
    cache so subsequent rounds resolve against real data.

No bundled snapshot ships with the package — the on-disk cache under
``~/.echo-agent/cache/models_dev.json`` is the offline-first store, written
after the first successful fetch.
"""

from __future__ import annotations

import json
import logging
import os
import tempfile
import threading
import time
from pathlib import Path

import httpx

from echo_agent.runtime_paths import echo_home

logger = logging.getLogger(__name__)

MODELS_DEV_URL = "https://models.dev/api.json"
_FETCH_TIMEOUT = 15.0
_CACHE_TTL = 3600  # seconds an in-memory / on-disk cache is considered fresh
_STALE_GRACE = 300  # after a failed refresh, retry this soon rather than in a full TTL

# echo-agent provider name -> models.dev provider ID. models.dev keys its
# catalog by its own provider IDs, which differ from ours for a few providers.
# Unmapped names are tried as-is (many already match), and lookup also falls
# back to scanning every provider when the provider is unknown/empty.
PROVIDER_TO_MODELS_DEV: dict[str, str] = {
    "openai": "openai",
    "anthropic": "anthropic",
    "gemini": "google",
    "google": "google",
    "deepseek": "deepseek",
    "minimax": "minimax",
    "qwen": "alibaba",
    "alibaba": "alibaba",
    "moonshot": "moonshot",
    "kimi": "moonshot",
    "openrouter": "openrouter",
    "xai": "xai",
    "grok": "xai",
}

# Module-level cache. Guarded by _lock for the refresh bookkeeping; reads of the
# dict itself are atomic enough for our purposes (single assignment swaps).
_cache: dict = {}
_cache_time: float = 0.0
_lock = threading.Lock()
_refreshing = False
_disk_loaded = False


def _cache_path() -> Path:
    return echo_home() / "cache" / "models_dev.json"


def _load_disk_cache() -> tuple[dict, float]:
    """Load the on-disk cache and its age. Returns ({}, 0) on any failure."""
    path = _cache_path()
    try:
        mtime = path.stat().st_mtime
        with path.open(encoding="utf-8") as f:
            data = json.load(f)
        if isinstance(data, dict) and data:
            return data, mtime
    except (OSError, ValueError):
        pass
    return {}, 0.0


def _save_disk_cache(data: dict) -> None:
    """Atomically persist the catalog to disk (compact). Best-effort."""
    path = _cache_path()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(data, f, ensure_ascii=False, separators=(",", ":"))
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, str(path))
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except OSError as e:
        logger.debug("models.dev disk cache write failed: %s", e)


def _fetch_from_network() -> dict:
    """Fetch and parse api.json. Returns {} on any network/parse failure."""
    try:
        resp = httpx.get(MODELS_DEV_URL, timeout=_FETCH_TIMEOUT)
        resp.raise_for_status()
        data = resp.json()
        if isinstance(data, dict) and data:
            return data
    except Exception as e:  # noqa: BLE001 - network/parse best-effort
        logger.debug("models.dev fetch failed: %s", e)
    return {}


def _do_refresh() -> None:
    """Network refresh body run in the daemon thread. Fills cache + disk."""
    global _cache, _cache_time, _refreshing
    try:
        data = _fetch_from_network()
        if data:
            _save_disk_cache(data)
            with _lock:
                _cache = data
                _cache_time = time.time()
        else:
            # Failed fetch: retry sooner than a full TTL by aging the stamp.
            with _lock:
                if _cache:
                    _cache_time = time.time() - _CACHE_TTL + _STALE_GRACE
    finally:
        with _lock:
            _refreshing = False


def _ensure_cache(*, blocking: bool = False) -> dict:
    """Return the freshest cache available without ever blocking on the network.

    Populates from disk on first use, then kicks off a fire-and-forget network
    refresh when the cache is empty or older than the TTL. The returned dict is
    whatever we have right now (possibly empty on a cold, offline first call).
    ``blocking=True`` is only for tests/tooling that want a synchronous refresh.
    """
    global _cache, _cache_time, _refreshing, _disk_loaded

    with _lock:
        if not _disk_loaded and not _cache:
            _disk_loaded = True
            disk, mtime = _load_disk_cache()
            if disk:
                _cache = disk
                _cache_time = mtime
        fresh = bool(_cache) and (time.time() - _cache_time) < _CACHE_TTL
        should_refresh = not fresh and not _refreshing
        if should_refresh:
            _refreshing = True
        current = _cache

    if not should_refresh:
        return current

    if blocking:
        _do_refresh()
        with _lock:
            return _cache

    threading.Thread(target=_do_refresh, name="models-dev-refresh", daemon=True).start()
    return current


def _extract_context(entry: dict) -> int:
    """Pull ``limit.context`` from a model entry. 0 when absent/invalid.

    models.dev nests the window under ``limit.context``; ``context`` <= 0
    (some audio/image models) is treated as unset.
    """
    if not isinstance(entry, dict):
        return 0
    limit = entry.get("limit")
    if not isinstance(limit, dict):
        return 0
    ctx = limit.get("context")
    if isinstance(ctx, (int, float)) and ctx > 0:
        return int(ctx)
    return 0


def _lookup_in_provider(models: dict, model: str) -> int:
    """Exact then case-insensitive model-id match within one provider's models."""
    entry = models.get(model)
    if isinstance(entry, dict):
        ctx = _extract_context(entry)
        if ctx:
            return ctx
    lowered = model.lower()
    for mid, entry in models.items():
        if isinstance(mid, str) and mid.lower() == lowered and isinstance(entry, dict):
            ctx = _extract_context(entry)
            if ctx:
                return ctx
    return 0


def lookup_context(model: str, provider: str = "", *, blocking: bool = False) -> int:
    """Resolve ``model``'s context window from models.dev. 0 on any miss.

    Never blocks on the network (unless ``blocking=True`` for tests): reads the
    cache, triggering an async refresh when stale. When ``provider`` maps to a
    known models.dev id we trust that provider's window directly. On an unknown/
    unmapped provider or a miss there, we scan every provider and return the
    consensus (most-reported) window rather than the first hit, so a single
    third-party mirror that mis-reports the window cannot decide the answer.
    """
    if not model:
        return 0
    data = _ensure_cache(blocking=blocking)
    if not data:
        return 0

    mdev_id = PROVIDER_TO_MODELS_DEV.get((provider or "").lower())
    if mdev_id:
        pdata = data.get(mdev_id)
        if isinstance(pdata, dict):
            models = pdata.get("models")
            if isinstance(models, dict):
                ctx = _lookup_in_provider(models, model)
                if ctx:
                    return ctx

    # Provider unknown, unmapped, or miss within the mapped provider: scan all.
    #
    # The same model id appears under dozens of third-party providers (mirrors,
    # aggregators, resellers) whose reported windows disagree — e.g. MiniMax-M3
    # shows up as 1000000 (six providers), 512000 (four), 524288 (one) and
    # 1048576 (one). Returning the FIRST hit means dict iteration order silently
    # decides the answer, so a single provider that mis-reports 512K can win over
    # the six that report the true 1M. Instead collect every hit and take the
    # mode (most-reported value); on a tie prefer the larger window, since an
    # under-report wrongly over-triggers compression.
    hits: list[int] = []
    for pdata in data.values():
        if not isinstance(pdata, dict):
            continue
        models = pdata.get("models")
        if isinstance(models, dict):
            ctx = _lookup_in_provider(models, model)
            if ctx:
                hits.append(ctx)
    return _consensus_window(hits)


def _consensus_window(hits: list[int]) -> int:
    """Pick the most-reported window from cross-provider hits; 0 if none.

    Ties break toward the larger value: an under-reported window makes the
    compressor trigger too early, which is the more harmful failure mode.
    """
    if not hits:
        return 0
    counts: dict[int, int] = {}
    for value in hits:
        counts[value] = counts.get(value, 0) + 1
    # max by (frequency, value) -> most common wins, larger value breaks ties.
    return max(counts, key=lambda v: (counts[v], v))


def _reset_for_tests() -> None:
    """Clear all module state so tests start from a clean slate."""
    global _cache, _cache_time, _refreshing, _disk_loaded
    with _lock:
        _cache = {}
        _cache_time = 0.0
        _refreshing = False
        _disk_loaded = False
