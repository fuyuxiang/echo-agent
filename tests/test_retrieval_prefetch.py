import time
from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.bus.events import InboundEvent
from echo_agent.memory.prefetch import (
    RetrievalCacheEntry,
    RetrievalPrefetcher,
    query_tokens,
    is_fresh,
)
from echo_agent.session.manager import Session


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


# --- ContextStage retrieval segment: cache-read + degrade/sync-on-miss ---


class _Mem:
    """Minimal scored memory object exposing .key / .content."""

    def __init__(self, key, content):
        self.key = key
        self.content = content


def _make_context_stage(*, cache, on_miss, on_retrieve=None, hybrid=True):
    """Build a ContextStage wired with a retrieval cache and on_miss policy.

    `on_retrieve` is a probe called with the query whenever inline retrieval
    runs (hybrid_retriever.retrieve when hybrid=True, else memory.search_scored).
    The probe returns the scored list to use.
    """
    config = MagicMock()
    config.session.max_history_messages = 100
    config.memory.enabled = True
    config.gateway.emit_progress_events = True
    config.gateway.progress_debug = False

    sessions = AsyncMock()
    sessions.save = AsyncMock()

    memory = MagicMock()
    memory.get_snapshot = MagicMock(return_value="")

    def _search_scored(text, limit=5, session_key=""):
        return on_retrieve(text) if on_retrieve else []

    memory.search_scored = MagicMock(side_effect=_search_scored)

    compressor = MagicMock()
    compressor.should_compress = MagicMock(return_value=False)

    # Capture the retrieval_context handed to build_messages so tests can
    # assert what made it into the prompt.
    captured = {}

    def _build_messages(**kwargs):
        captured["retrieval_context"] = kwargs.get("retrieval_context", "")
        return [{"role": "user", "content": kwargs.get("current_message", "")}]

    context_builder = MagicMock()
    context_builder.build_system_prompt = MagicMock(return_value="sys")
    context_builder.build_messages = MagicMock(side_effect=_build_messages)

    inference = MagicMock()
    inference.filter_tools = MagicMock(return_value=[])

    hybrid_retriever = None
    if hybrid:
        hybrid_retriever = MagicMock()

        async def _retrieve(text, limit=5, session_key=""):
            return on_retrieve(text) if on_retrieve else []

        hybrid_retriever.retrieve = AsyncMock(side_effect=_retrieve)

    stage = ContextStage(
        config=config,
        sessions=sessions,
        memory=memory,
        compressor=compressor,
        context_builder=context_builder,
        skill_store=None,
        knowledge=None,
        hybrid_retriever=hybrid_retriever,
        planner=None,
        inference=inference,
        working_memories=OrderedDict(),
        memory_snapshots=OrderedDict(),
        snapshot_enabled=False,
        tool_definitions_fn=lambda: [],
        retrieval_cache_get=lambda sk: cache.get(sk),
        retrieval_on_miss=on_miss,
        cache_ttl=60.0,
        cache_jaccard_min=0.3,
    )
    return stage, captured, hybrid_retriever, memory


async def _stage_build(stage, *, session_key, text):
    event = InboundEvent.text_message(
        channel="cli", sender_id="u1", chat_id="c1", text=text
    )
    event.session_key_override = session_key
    session = Session(key=session_key)
    return await stage.build(
        event, session,
        publish_response=False, trace_id="t1",
        stream_publisher=None, intro_text="",
    )


@pytest.mark.asyncio
async def test_context_uses_fresh_cache():
    entry = RetrievalCacheEntry(
        query_text="deploy gateway",
        query_tokens=query_tokens("deploy gateway"),
        scored=[(_Mem("k1", "cached!"), 0.9)],
        created_at=time.time(),
    )
    stage, captured, hybrid, memory = _make_context_stage(
        cache={"sess-1": entry}, on_miss="degrade"
    )
    await _stage_build(stage, session_key="sess-1", text="gateway deploy steps")
    assert "cached!" in captured["retrieval_context"]
    hybrid.retrieve.assert_not_called()
    memory.search_scored.assert_not_called()


@pytest.mark.asyncio
async def test_cli_miss_degrades_to_empty():
    stage, captured, hybrid, memory = _make_context_stage(
        cache={}, on_miss="degrade"
    )
    await _stage_build(stage, session_key="new", text="anything")
    assert captured["retrieval_context"] == ""
    hybrid.retrieve.assert_not_called()
    memory.search_scored.assert_not_called()


@pytest.mark.asyncio
async def test_daemon_miss_syncs():
    calls = []

    def _probe(query):
        calls.append(query)
        return [(_Mem("k2", "fresh-sync"), 0.8)]

    stage, captured, hybrid, memory = _make_context_stage(
        cache={}, on_miss="sync", on_retrieve=_probe
    )
    await _stage_build(stage, session_key="new", text="anything")
    assert calls == ["anything"]
    assert "fresh-sync" in captured["retrieval_context"]


@pytest.mark.asyncio
async def test_daemon_miss_syncs_via_memory_when_no_hybrid():
    calls = []

    def _probe(query):
        calls.append(query)
        return [(_Mem("k3", "store-sync"), 0.7)]

    stage, captured, hybrid, memory = _make_context_stage(
        cache={}, on_miss="sync", on_retrieve=_probe, hybrid=False
    )
    await _stage_build(stage, session_key="new", text="anything")
    assert calls == ["anything"]
    assert "store-sync" in captured["retrieval_context"]


@pytest.mark.asyncio
async def test_stale_cache_misses_then_degrades():
    # Same topic but expired TTL → miss → CLI degrade → empty, no retrieve.
    entry = RetrievalCacheEntry(
        query_text="deploy gateway",
        query_tokens=query_tokens("deploy gateway"),
        scored=[(_Mem("k1", "stale!"), 0.9)],
        created_at=time.time() - 120,
    )
    stage, captured, hybrid, memory = _make_context_stage(
        cache={"sess-1": entry}, on_miss="degrade"
    )
    await _stage_build(stage, session_key="sess-1", text="deploy gateway")
    assert captured["retrieval_context"] == ""
    hybrid.retrieve.assert_not_called()
