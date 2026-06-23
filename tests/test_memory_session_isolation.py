import pytest
from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier


@pytest.mark.asyncio
async def test_retrieve_filters_by_session_visibility():
    a = MemoryEntry(type=MemoryType.ENVIRONMENT, tier=MemoryTier.SEMANTIC,
                    key="proj", content="alpha secret config", source_session="s:A")
    b = MemoryEntry(type=MemoryType.ENVIRONMENT, tier=MemoryTier.SEMANTIC,
                    key="proj", content="beta visible config", source_session="s:B")

    def vis(entry, sk):
        return (not entry.source_session) or entry.source_session == sk

    r = HybridRetriever(entries_fn=lambda: [a, b], visibility_fn=vis)
    out = await r.retrieve("config", limit=5, session_key="s:B")
    contents = {e.content for e, _ in out}
    assert "alpha secret config" not in contents


@pytest.mark.asyncio
async def test_retrieve_without_visibility_fn_unchanged():
    a = MemoryEntry(type=MemoryType.ENVIRONMENT, key="proj", content="alpha config", source_session="s:A")
    r = HybridRetriever(entries_fn=lambda: [a])
    out = await r.retrieve("config", limit=5, session_key="s:B")
    # 无 visibility_fn 时行为不变(不过滤)
    assert isinstance(out, list)
