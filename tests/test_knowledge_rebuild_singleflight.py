"""Knowledge rebuild single-flight — two concurrent calls must coalesce.

Reviewer P2 (file review): ``rebuild_async`` crosses ``await`` points, so the
``threading.Lock`` that protected the synchronous halves let two concurrent
callers race on the vector sidecar. The fix is an ``asyncio.Lock`` plus a
shared future: the second caller joins the first rebuild and gets the same
result instead of starting a parallel backfill.

The race is data-integrity, not a performance issue: A could write a sidecar
based on a stale snapshot of ``self._chunks`` while B was already mid-write,
silently losing a chunk or two. Verifying it directly requires mocking the
embedding path so both calls are paused at the same await point.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.knowledge.index import KnowledgeIndex


def _make_index(tmp_path: Path) -> KnowledgeIndex:
    docs = tmp_path / "docs"
    docs.mkdir()
    for i in range(5):
        (docs / f"d{i}.md").write_text(f"# Doc {i}\n" + ("alpha content " * 20), encoding="utf-8")
    return KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json")


def _attach_vector_store(index: KnowledgeIndex):
    """Hook a minimal vector store so rebuild_async takes the await path.

    The placeholder embed below is replaced by ``slow_embed`` in the test
    that exercises this helper; we pass it through so ``attach_embedding``
    gets a callable it can store without warning about an unawaited coroutine.
    """
    async def placeholder(text: str) -> list[float]:
        return [0.0, 0.0, 0.0]
    store = MagicMock()
    store.available = True
    store.load = MagicMock(return_value={})
    store.content_hashes = MagicMock(return_value={})
    store.build = MagicMock()
    store.save = MagicMock()
    index.attach_embedding(placeholder, dimensions=3, embed_timeout=2.0)


# ── coalescing ──────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_concurrent_rebuilds_share_a_single_execution(tmp_path):
    """Two callers waiting on rebuild_async must produce exactly one
    underlying rebuild, not two. The second caller joins the in-flight
    future and gets the same result.
    """
    index = _make_index(tmp_path)
    index.ensure_ready()

    # ``rebuild_async`` calls the synchronous ``rebuild`` via run_in_executor
    # when no vector store is attached, so the test entry point must be a
    # blocking callable. We use a future + sleep to pause it on the loop
    # thread so the second caller actually overlaps.
    call_count = 0
    loop = asyncio.get_running_loop()
    async def trigger_concurrent_b_release():
        nonlocal call_count
        call_count += 1
        await asyncio.sleep(0.05)
        return {"documents": index.doc_count, "chunks": index.chunk_count, "n": call_count}

    def slow_rebuild():
        return asyncio.run_coroutine_threadsafe(trigger_concurrent_b_release(), loop).result()

    index.rebuild = slow_rebuild

    a, b = await asyncio.gather(index.rebuild_async(), index.rebuild_async())

    assert call_count == 1, f"expected single execution, got {call_count}"
    assert a == b


@pytest.mark.asyncio
async def test_concurrent_rebuilds_with_embed_take_one_pass(tmp_path):
    """Same coalescing guarantee on the with-embed branch. The lock + shared
    future must hold across the executor await AND the per-chunk embed awaits.
    """
    index = _make_index(tmp_path)
    index.ensure_ready()
    _attach_vector_store(index)

    # Slow the embed so the second caller actually overlaps.
    embed_calls = 0
    release = asyncio.Event()
    async def slow_embed(text: str) -> list[float]:
        nonlocal embed_calls
        embed_calls += 1
        if embed_calls >= 2:
            release.set()
        await asyncio.sleep(0.05)
        return [0.1, 0.2, 0.3]
    index._embed_fn = slow_embed

    a, b = await asyncio.gather(index.rebuild_async(), index.rebuild_async())

    # If single-flight held, only the first rebuild embeds each chunk once.
    # If it failed, both rebuilds would race and embed_calls would be 2x.
    assert embed_calls == index.chunk_count, (
        f"expected {index.chunk_count} embeds (one per chunk, single rebuild), "
        f"got {embed_calls} (concurrent rebuilds were not coalesced)"
    )
    assert a == b


# ── sequential correctness still holds ────────────────────────────────────────


@pytest.mark.asyncio
async def test_sequential_rebuilds_each_run_a_full_pass(tmp_path):
    """After one rebuild completes, a later caller must start a fresh one —
    coalescing is only for *concurrent* callers.
    """
    index = _make_index(tmp_path)
    index.ensure_ready()

    call_count = 0
    loop = asyncio.get_running_loop()
    async def run_once():
        nonlocal call_count
        call_count += 1
        return {"documents": 0, "chunks": 0, "n": call_count}
    def counting_rebuild():
        return asyncio.run_coroutine_threadsafe(run_once(), loop).result()
    index.rebuild = counting_rebuild

    first = await index.rebuild_async()
    second = await index.rebuild_async()

    assert call_count == 2
    assert first["n"] == 1
    assert second["n"] == 2


# ── exception isolation ─────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_failed_rebuild_unblocks_subsequent_calls(tmp_path):
    """If the in-flight rebuild raises, its future records the exception.
    The next caller must be able to start a fresh rebuild — the failed
    future should be cleared rather than poison the queue forever.
    """
    index = _make_index(tmp_path)
    index.ensure_ready()

    attempts = 0
    loop = asyncio.get_running_loop()
    async def maybe_fail():
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise RuntimeError("transient failure")
        return {"documents": 0, "chunks": 0, "ok": True}
    def flaky_rebuild():
        return asyncio.run_coroutine_threadsafe(maybe_fail(), loop).result()
    index.rebuild = flaky_rebuild

    with pytest.raises(RuntimeError, match="transient"):
        await index.rebuild_async()
    # The failed future was cleared; a second caller proceeds with a fresh rebuild.
    result = await index.rebuild_async()
    assert result["ok"] is True