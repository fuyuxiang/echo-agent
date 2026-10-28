import pytest
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.service import MemoryService, ActorContext
from echo_agent.memory.types import MemoryType

@pytest.mark.asyncio
async def test_append_version_and_invalidation(tmp_path):
    calls = []
    async def _inval(scope, g): calls.append((scope, g))
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    svc = MemoryService(store, invalidate_fn=_inval)
    ctx = ActorContext(actor="model", session_key="s", memory_scope="x")
    await svc.add(ctx, type=MemoryType.USER, key="home", content="北京", source="user_stated")
    r = await svc.add(ctx, type=MemoryType.USER, key="home", content="上海", source="user_stated")
    live = [e for e in store._entries.values() if e.key == "home" and not e.is_superseded]
    assert len(live) == 1 and live[0].content == "上海" and r.entry.version == 2
    assert ("x", False) in calls

def test_snapshot_no_md_segment(tmp_path):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    store.write_long_term("", "## legacy\n\n- **old**: 不该进prompt")
    snap, _ = store.get_snapshot_with_ids(session_key="")
    assert "## Long-term Memory" not in snap and "不该进prompt" not in snap
