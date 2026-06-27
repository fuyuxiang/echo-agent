import time

import pytest

from echo_agent.memory.prefetch import (
    RetrievalCacheEntry,
    RetrievalPrefetcher,
    query_tokens,
    is_fresh,
)


def _entry(text, scored=None, created=None):
    return RetrievalCacheEntry(
        query_text=text, query_tokens=query_tokens(text),
        scored=scored or [], created_at=created if created is not None else time.time(),
    )


def test_fresh_when_recent_and_similar():
    e = _entry("how to deploy the gateway service")
    assert is_fresh(e, "deploy gateway service steps", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_stale_when_expired():
    e = _entry("deploy gateway", created=time.time() - 120)
    assert not is_fresh(e, "deploy gateway", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_miss_when_topic_shifts():
    e = _entry("how to deploy the gateway service")
    assert not is_fresh(e, "what is my cat's name", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_empty_tokens_not_fresh():
    # Empty query tokens must not divide by zero and must miss.
    e = _entry("deploy gateway")
    assert not is_fresh(e, "", now=time.time(), ttl=60.0, jaccard_min=0.3)
    empty = _entry("")
    assert not is_fresh(empty, "deploy gateway", now=time.time(), ttl=60.0, jaccard_min=0.3)


def test_cjk_query_tokens():
    # Chinese queries tokenize via cjk_tokens (chars + bigrams), so a repeated
    # CJK query stays fresh.
    e = _entry("如何部署网关服务")
    assert is_fresh(e, "如何部署网关服务", now=time.time(), ttl=60.0, jaccard_min=0.3)


@pytest.mark.asyncio
async def test_prefetcher_writes_cache_entry():
    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            return [("entry-obj", 0.9)]

    written = {}

    async def cache_put(sk, entry):
        written[sk] = entry

    pf = RetrievalPrefetcher(_R(), cache_put, limit=5)
    await pf.prefetch("sess-1", "deploy gateway")
    assert "sess-1" in written
    e = written["sess-1"]
    assert isinstance(e, RetrievalCacheEntry)
    assert e.query_text == "deploy gateway"
    assert e.scored == [("entry-obj", 0.9)]


@pytest.mark.asyncio
async def test_prefetcher_passes_limit_and_session_key():
    seen = {}

    class _R:
        async def retrieve(self, query, limit=10, session_key=""):
            seen["query"] = query
            seen["limit"] = limit
            seen["session_key"] = session_key
            return []

    async def cache_put(sk, entry):
        pass

    pf = RetrievalPrefetcher(_R(), cache_put, limit=3)
    await pf.prefetch("sess-9", "deploy gateway")
    assert seen == {"query": "deploy gateway", "limit": 3, "session_key": "sess-9"}


@pytest.mark.asyncio
async def test_prefetcher_swallows_retrieve_failure():
    # A background prefetch failure must not propagate; next turn just misses.
    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            raise RuntimeError("retriever down")

    called = False

    async def cache_put(sk, entry):
        nonlocal called
        called = True

    pf = RetrievalPrefetcher(_R(), cache_put, limit=5)
    await pf.prefetch("sess-1", "deploy gateway")
    assert called is False
