# tests/test_retrieval_eligibility.py
import pytest
from unittest.mock import AsyncMock, MagicMock
from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType


def _entry(eid, content, **kw):
    e = MemoryEntry(type=MemoryType.USER, key=eid, content=content, **kw)
    e.id = eid
    return e


@pytest.mark.asyncio
async def test_superseded_not_in_candidate_pool(monkeypatch):
    live = _entry("live", "上海住址", source="user_stated")
    old = _entry("old", "北京住址", source="user_stated", superseded_by="live")
    vec = MagicMock()
    vec.search = AsyncMock(return_value=[])
    r = HybridRetriever(
        entries_fn=lambda: [live, old],
        vector_index=vec,
        embed_fn=AsyncMock(return_value=[0.1]),
        visibility_fn=lambda e, s: True,
        is_unresolved_fn=lambda _id: False,
    )
    hits = await r.retrieve("北京", memory_scope="s1", limit=5)
    assert all(h.id != "old" for h in hits)


@pytest.mark.asyncio
async def test_archived_excluded(monkeypatch):
    arch = _entry("a", "旧档案", source="user_stated", tier=MemoryTier.ARCHIVAL)
    vec = MagicMock(); vec.search = AsyncMock(return_value=[])
    r = HybridRetriever(
        entries_fn=lambda: [arch], vector_index=vec,
        embed_fn=AsyncMock(return_value=[0.1]),
        visibility_fn=lambda e, s: True, is_unresolved_fn=lambda _id: False,
    )
    hits = await r.retrieve("档案", memory_scope="s1", limit=5)
    assert hits == []
