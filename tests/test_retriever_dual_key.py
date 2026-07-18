from __future__ import annotations

import asyncio

from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.types import MemoryEntry, MemoryType, Episode


def _entry(eid, key, content, source_session):
    e = MemoryEntry(id=eid, type=MemoryType.USER, key=key, content=content)
    e.source_session = source_session
    return e


def test_retrieve_uses_memory_scope_for_visibility_and_session_for_episodes():
    owner_entry = _entry("e1", "pref", "喜欢Python", "owner")
    # 可见性: 只有 source_session=="owner" 的条目可见(模拟 store 严格相等)
    def visibility(e, scope):
        return e.source_session == scope
    seen_episode_key = {}
    async def episode_search(query, ep_key, limit):
        seen_episode_key["key"] = ep_key
        return [Episode(id="ep1", session_key=ep_key, summary="讨论部署")]
    r = HybridRetriever(
        entries_fn=lambda: [owner_entry],
        visibility_fn=visibility,
        episode_search_fn=episode_search,
    )
    scored = asyncio.run(r.retrieve(
        "python", limit=8, memory_scope="owner", episode_session_key="telegram:alice",
    ))
    # 语义命中(owner 可见) + episode 用的是 session 键
    assert any(getattr(s[0], "key", "") == "pref" for s in scored)
    assert seen_episode_key["key"] == "telegram:alice"


def test_retrieve_scope_mismatch_hides_semantic():
    owner_entry = _entry("e1", "pref", "喜欢Python", "owner")
    def visibility(e, scope):
        return e.source_session == scope
    r = HybridRetriever(entries_fn=lambda: [owner_entry], visibility_fn=visibility)
    scored = asyncio.run(r.retrieve(
        "python", limit=8, memory_scope="telegram:bob", episode_session_key="telegram:bob",
    ))
    assert not any(getattr(s[0], "key", "") == "pref" for s in scored)
