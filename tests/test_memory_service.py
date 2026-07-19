import pytest
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.service import MemoryService, ActorContext
from echo_agent.memory.types import MemoryEntry, MemoryType


def _svc(tmp_path, **kw):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    return MemoryService(store, **kw), store


@pytest.mark.asyncio
async def test_add_user_requires_scope(tmp_path):
    svc, _ = _svc(tmp_path)
    r = await svc.add(ActorContext(actor="model", session_key="s", memory_scope=""),
                      type=MemoryType.USER, key="k", content="c", source="model_inferred")
    assert r.ok is False and r.reason == "rejected_scope"


@pytest.mark.asyncio
async def test_low_priority_replace_rejected_no_contradiction(tmp_path):
    svc, store = _svc(tmp_path)
    e = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                              source="user_stated", source_session="scope1"))
    r = await svc.replace(ActorContext(actor="model", session_key="s", memory_scope="scope1"),
                          e.id, content="北京", source="model_inferred")
    assert r.ok is False and r.reason == "rejected_provenance"
    assert store.get(e.id).content == "上海"
    # 不写 contradiction：无 suspected_conflict tag
    assert "suspected_conflict" not in store.get(e.id).tags


@pytest.mark.asyncio
async def test_env_write_denied_for_model_by_default(tmp_path):
    svc, _ = _svc(tmp_path, allow_env_writes=False)
    r = await svc.add(ActorContext(actor="model", session_key="s", memory_scope="scope1"),
                      type=MemoryType.ENVIRONMENT, key="os", content="Linux", source="model_inferred")
    assert r.ok is False and r.reason == "rejected_env"


@pytest.mark.asyncio
async def test_flush_before_invalidate(tmp_path):
    order = []
    async def _flush(): order.append("flush"); return 0
    async def _inval(scope, g): order.append("invalidate")
    svc, _ = _svc(tmp_path, flush_fn=_flush, invalidate_fn=_inval)
    await svc.add(ActorContext(actor="model", session_key="s", memory_scope="scope1"),
                  type=MemoryType.USER, key="k", content="c", source="user_stated")
    assert order == ["flush", "invalidate"]
