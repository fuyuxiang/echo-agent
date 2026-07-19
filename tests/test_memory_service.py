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
async def test_add_invalid_content_rejected(tmp_path):
    # 非法内容(空白)由 store.add 内部校验抛 ValueError,service 捕获转 invalid;
    # service 层不再重复预校验(store 是唯一校验源)。空白内容仍被拒、不落库。
    svc, store = _svc(tmp_path)
    r = await svc.add(ActorContext(actor="model", session_key="s", memory_scope="scope1"),
                      type=MemoryType.USER, key="k", content="   ", source="model_inferred")
    assert r.ok is False and r.reason == "invalid"
    assert store.find_by_key("k", session_key="scope1") is None


@pytest.mark.asyncio
async def test_replace_invalid_content_rejected(tmp_path):
    # 非法内容由 store.update 内部校验抛 ValueError,service 捕获转 invalid,原内容保留。
    svc, store = _svc(tmp_path)
    e = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                              source="user_stated", source_session="scope1"))
    r = await svc.replace(ActorContext(actor="admin", session_key="s", memory_scope="scope1"),
                          e.id, content="   ", source="admin", override=True)
    assert r.ok is False and r.reason == "invalid"
    assert store.get(e.id).content == "上海"


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


@pytest.mark.asyncio
async def test_public_invalidate_forwards_to_fn(tmp_path):
    # 裁决路径(如工具 resolve_contradiction)绕过八步写序直接改 store,
    # 需要一个公开失效钩子;它应把 (scope, global_scope) 透传给 invalidate_fn。
    calls = []
    async def _inval(scope, g): calls.append((scope, g))
    svc, _ = _svc(tmp_path, invalidate_fn=_inval)
    await svc.invalidate("scope1", global_scope=True)
    assert calls == [("scope1", True)]


@pytest.mark.asyncio
async def test_public_invalidate_noop_without_fn(tmp_path):
    # 未注入 invalidate_fn 时公开失效应安全跳过,不抛异常。
    svc, _ = _svc(tmp_path)
    await svc.invalidate("scope1", global_scope=True)


@pytest.mark.asyncio
async def test_maintenance_remove_deletes_user_stated_archival(tmp_path):
    # 决策2:maintenance 是内部维护(归档删除),走 remove 应跳过 provenance,
    # 能删掉 user_stated 的高优先级归档条目——与它 set_tier/maintenance_update
    # 的免检身份一致。此前 maintenance 映射 legacy(rank0),会被 provenance 拦下。
    svc, store = _svc(tmp_path)
    e = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                              source="user_stated", source_session="scope1"))
    r = await svc.remove(
        ActorContext(actor="maintenance", session_key="scope1", memory_scope="scope1"),
        e.id,
    )
    assert r.ok is True and r.reason == ""
    assert store.get(e.id) is None
