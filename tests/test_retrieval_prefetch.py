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


def _make_context_stage(*, cache, on_miss, on_retrieve=None, hybrid=True,
                         episodic=None, knowledge=None):
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
        knowledge=knowledge,
        hybrid_retriever=hybrid_retriever,
        planner=None,
        inference=inference,
        working_memories=OrderedDict(),
        memory_snapshots=OrderedDict(),
        snapshot_enabled=False,
        tool_definitions_fn=lambda: [],
        episodic=episodic,
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


# --- ResponseStage post-reply prefetch trigger (Task 12) ---


class _FakePrefetcher:
    """Stand-in for RetrievalPrefetcher whose prefetch returns a real coroutine
    so the spawn capture can close it without warnings."""

    def __init__(self):
        self.calls = []

    async def prefetch(self, session_key, query, user_id=""):
        self.calls.append((session_key, query, user_id))


def _make_finalize_stage(prefetcher=None, spawn=None):
    from echo_agent.agent.pipeline.response_stage import ResponseStage

    sessions = AsyncMock()
    sessions.save = AsyncMock()

    memory = MagicMock()
    memory.has_pending_embeds = MagicMock(return_value=False)

    # spec=[] => no auto-created attributes, so the `hasattr(_consolidator)`
    # consolidation branch is skipped in finalize.
    consolidation = MagicMock(spec=[])

    return ResponseStage(
        config=MagicMock(),
        sessions=sessions,
        memory=memory,
        provider=MagicMock(),
        consolidation_worker=consolidation,
        default_model="m",
        spawn_fn=spawn or (lambda coro, **kw: getattr(coro, "close", lambda: None)()),
        clear_memory_snapshot_fn=AsyncMock(),
        skill_store=None,
        working_memories=None,
        prefetcher=prefetcher,
    )


async def _response_finalize(stage, *, session_key, text):
    from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext

    event = InboundEvent.text_message(
        channel="cli", sender_id="u1", chat_id="c1", text=text
    )
    event.session_key_override = session_key
    session = Session(key=session_key)
    ctx = PipelineContext(
        event=event, session=session, trace_id="t1", publish_response=False
    )
    return await stage.finalize(ctx, InferenceResult(response_text="ok"))


@pytest.mark.asyncio
async def test_finalize_schedules_prefetch_as_discardable():
    from echo_agent.agent.background import Tier

    spawned = []
    coros = []

    def spawn(coro, *, session_key="", tier=None):
        spawned.append((tier, session_key))
        coros.append(coro)

    pf = _FakePrefetcher()
    stage = _make_finalize_stage(prefetcher=pf, spawn=spawn)
    await _response_finalize(stage, session_key="sess-1", text="deploy gateway")

    assert any(t == Tier.DISCARDABLE for t, _ in spawned)
    # Awaiting the scheduled coroutine drives the real prefetch call, proving
    # it was wired with this turn's session_key + query.
    await coros[0]
    assert pf.calls[0][:2] == ("sess-1", "deploy gateway")


@pytest.mark.asyncio
async def test_finalize_skips_prefetch_when_no_prefetcher():
    spawned = []

    def spawn(coro, *, session_key="", tier=None):
        spawned.append((tier, session_key))
        getattr(coro, "close", lambda: None)()

    stage = _make_finalize_stage(prefetcher=None, spawn=spawn)
    await _response_finalize(stage, session_key="sess-1", text="deploy gateway")
    assert spawned == []


@pytest.mark.asyncio
async def test_finalize_skips_prefetch_when_empty_text():
    spawned = []

    def spawn(coro, *, session_key="", tier=None):
        spawned.append((tier, session_key))
        getattr(coro, "close", lambda: None)()

    stage = _make_finalize_stage(prefetcher=_FakePrefetcher(), spawn=spawn)
    await _response_finalize(stage, session_key="sess-1", text="")
    assert spawned == []


@pytest.mark.asyncio
async def test_prefetch_cache_isolated_per_session_end_to_end():
    # End-to-end: run the prefetcher through the REAL loop cache writer
    # (_put_retrieval_cache -> _lru_put) and reader (_get_retrieval_cache),
    # then assert session A's prefetched entry is invisible to session B.
    import asyncio

    from echo_agent.agent.loop import AgentLoop

    host = type("_CacheHost", (), {})()
    host._retrieval_cache = OrderedDict()
    host._state_lock = asyncio.Lock()
    host._max_cached_sessions = 200
    host._lru_put = AgentLoop._lru_put.__get__(host)
    host._put_retrieval_cache = AgentLoop._put_retrieval_cache.__get__(host)
    host._get_retrieval_cache = AgentLoop._get_retrieval_cache.__get__(host)

    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            return [(f"hit-for-{session_key}", 0.9)]

    pf = RetrievalPrefetcher(_R(), host._put_retrieval_cache, limit=5)
    await pf.prefetch("sess-A", "deploy gateway")

    entry_a = host._get_retrieval_cache("sess-A")
    assert entry_a is not None
    assert entry_a.scored == [("hit-for-sess-A", 0.9)]
    # session B never prefetched -> must not see A's entry.
    assert host._get_retrieval_cache("sess-B") is None


# --- Task 13: episodic + knowledge folded into the same prefetch/cache ---


@pytest.mark.asyncio
async def test_prefetcher_populates_episodes_and_knowledge():
    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            return [("m", 0.9)]

    async def _epi(query, session_key):
        return [type("E", (), {"summary": "ep1"})()]

    def _know(query, user_id):  # sync, CPU-bound in prod
        return "KB: relevant doc"

    written = {}

    async def cache_put(sk, e):
        written[sk] = e

    pf = RetrievalPrefetcher(
        _R(), cache_put, limit=5, episodic_fetch=_epi, knowledge_fetch=_know
    )
    await pf.prefetch("s1", "deploy gateway")
    e = written["s1"]
    assert e.scored == [("m", 0.9)]
    assert e.episodes and e.episodes[0].summary == "ep1"
    assert e.knowledge_context == "KB: relevant doc"


@pytest.mark.asyncio
async def test_prefetcher_passes_user_id_and_session_to_subfetchers():
    # session isolation: episodic gets session_key, knowledge gets user_id.
    seen = {}

    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            return []

    async def _epi(query, session_key):
        seen["epi"] = (query, session_key)
        return []

    def _know(query, user_id):
        seen["know"] = (query, user_id)
        return ""

    async def cache_put(sk, e):
        pass

    pf = RetrievalPrefetcher(
        _R(), cache_put, limit=5, episodic_fetch=_epi, knowledge_fetch=_know
    )
    await pf.prefetch("sess-X", "deploy gateway", user_id="user-7")
    assert seen["epi"] == ("deploy gateway", "sess-X")
    assert seen["know"] == ("deploy gateway", "user-7")


@pytest.mark.asyncio
async def test_prefetcher_subfetch_failures_isolated():
    # episodic failure must not block knowledge and vice versa; both leave None
    # without crashing the background task.
    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            return [("m", 0.5)]

    async def _epi(query, session_key):
        raise RuntimeError("episodic db down")

    def _know(query, user_id):
        raise RuntimeError("knowledge scan blew up")

    written = {}

    async def cache_put(sk, e):
        written[sk] = e

    pf = RetrievalPrefetcher(
        _R(), cache_put, limit=5, episodic_fetch=_epi, knowledge_fetch=_know
    )
    await pf.prefetch("s2", "deploy gateway")
    e = written["s2"]
    assert e.scored == [("m", 0.5)]
    assert e.episodes is None
    assert e.knowledge_context is None


@pytest.mark.asyncio
async def test_prefetcher_knowledge_runs_off_event_loop():
    # The sync CPU-bound knowledge fetch must run in an executor thread, not on
    # the event loop thread — otherwise the background prefetch still blocks.
    import threading

    loop_thread = threading.get_ident()
    seen = {}

    class _R:
        async def retrieve(self, query, limit=5, session_key=""):
            return []

    def _know(query, user_id):
        seen["thread"] = threading.get_ident()
        return "kb"

    async def cache_put(sk, e):
        pass

    pf = RetrievalPrefetcher(_R(), cache_put, limit=5, knowledge_fetch=_know)
    await pf.prefetch("s3", "deploy gateway")
    assert seen["thread"] != loop_thread


# --- ContextStage episodic/knowledge segments: cache-read + on_miss policy ---


class _Episode:
    def __init__(self, summary):
        self.summary = summary


@pytest.mark.asyncio
async def test_context_uses_cached_episodes_and_knowledge():
    episodic = MagicMock()
    episodic.search_episodes = AsyncMock(side_effect=AssertionError("must not query"))
    episodic.get_session_episodes = AsyncMock(side_effect=AssertionError("must not query"))
    knowledge = MagicMock()
    knowledge.search = MagicMock(side_effect=AssertionError("must not scan"))
    knowledge.format_results = MagicMock(side_effect=AssertionError("must not format"))

    entry = RetrievalCacheEntry(
        query_text="deploy gateway",
        query_tokens=query_tokens("deploy gateway"),
        scored=[],
        created_at=time.time(),
        episodes=[_Episode("cached episode")],
        knowledge_context="cached KB block",
    )
    stage, captured, hybrid, memory = _make_context_stage(
        cache={"sess-1": entry}, on_miss="degrade",
        episodic=episodic, knowledge=knowledge,
    )
    await _stage_build(stage, session_key="sess-1", text="gateway deploy steps")
    ctx = captured["retrieval_context"]
    assert "Past episodes:" in ctx
    assert "cached episode" in ctx
    assert "cached KB block" in ctx


@pytest.mark.asyncio
async def test_context_episode_knowledge_degrade_on_miss():
    episodic = MagicMock()
    episodic.search_episodes = AsyncMock(side_effect=AssertionError("must not query"))
    episodic.get_session_episodes = AsyncMock(side_effect=AssertionError("must not query"))
    knowledge = MagicMock()
    knowledge.search = MagicMock(side_effect=AssertionError("must not scan"))

    stage, captured, hybrid, memory = _make_context_stage(
        cache={}, on_miss="degrade", episodic=episodic, knowledge=knowledge,
    )
    await _stage_build(stage, session_key="new", text="anything")
    assert "Past episodes:" not in captured["retrieval_context"]
    assert "cached KB" not in captured["retrieval_context"]


@pytest.mark.asyncio
async def test_context_episode_knowledge_sync_on_miss():
    episodic = MagicMock()
    episodic.search_episodes = AsyncMock(return_value=[_Episode("synced episode")])
    episodic.get_session_episodes = AsyncMock(return_value=[])
    knowledge = MagicMock()
    knowledge.search = MagicMock(return_value=[object()])
    knowledge.format_results = MagicMock(return_value="synced KB block")

    stage, captured, hybrid, memory = _make_context_stage(
        cache={}, on_miss="sync", episodic=episodic, knowledge=knowledge,
    )
    await _stage_build(stage, session_key="new", text="anything")
    ctx = captured["retrieval_context"]
    assert "synced episode" in ctx
    assert "synced KB block" in ctx
    knowledge.search.assert_called_once()
