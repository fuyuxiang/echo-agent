# tests/test_r2_baseline_e2e.py
import pytest
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.service import MemoryService, ActorContext
from echo_agent.memory.types import MemoryType
from echo_agent.memory.eligibility import Audience

@pytest.mark.asyncio
async def test_append_version_chain_intact(tmp_path):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    svc = MemoryService(store)
    ctx = ActorContext(actor="model", session_key="s", memory_scope="x")
    await svc.add(ctx, type=MemoryType.USER, key="home", content="北京", source="user_stated")
    r = await svc.add(ctx, type=MemoryType.USER, key="home", content="上海", source="user_stated")
    live = [e for e in store._entries.values() if e.key == "home" and not e.is_superseded]
    assert len(live) == 1 and live[0].content == "上海" and r.entry.version == 2
    hits = store.search_scored("北京", session_key="x", audience=Audience.TOOL)
    assert all("北京" not in e.content for e, _ in hits)  # superseded 不召回

@pytest.mark.asyncio
async def test_service_invalidation_intact(tmp_path):
    calls = []
    async def _inval(scope, g): calls.append((scope, g))
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    svc = MemoryService(store, invalidate_fn=_inval)
    await svc.add(ActorContext(actor="model", session_key="s", memory_scope="x"),
                  type=MemoryType.USER, key="k", content="v", source="user_stated")
    assert ("x", False) in calls
