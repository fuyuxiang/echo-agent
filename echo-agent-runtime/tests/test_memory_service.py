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
    async def _flush():
        order.append("flush")
        return 0
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


@pytest.mark.asyncio
async def test_service_add_same_key_appends_version(tmp_path):
    # 经 service.add 的同 key 改口端到端应透明获得 store 层 append-version:
    # version 递增、旧版本保留(superseded)、WriteResult.entry 返回新版本、
    # live(active)只留最新一条。service 无需新逻辑,只透传 store.add 返回值。
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.service import MemoryService, ActorContext
    from echo_agent.memory.types import MemoryType
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    svc = MemoryService(store)
    ctx = ActorContext(actor="model", session_key="s", memory_scope="x")
    await svc.add(ctx, type=MemoryType.USER, key="home", content="北京", source="user_stated")
    r = await svc.add(ctx, type=MemoryType.USER, key="home", content="上海", source="user_stated")
    assert r.ok is True and r.entry.version == 2
    live = [e for e in store._entries.values() if e.key == "home" and not e.is_superseded]
    assert len(live) == 1 and live[0].content == "上海"


@pytest.mark.asyncio
async def test_admin_correction_cannot_be_overwritten_by_background_extraction(tmp_path):
    svc, store = _svc(tmp_path)
    ctx = ActorContext(actor="admin", memory_scope="owner")
    entry = store.add(MemoryEntry(key="drink", content="coffee", source_session="owner"))
    corrected = await svc.replace(ctx, entry.id, content="tea", source="admin", override=True)
    assert corrected.ok
    await svc.add(ActorContext(actor="reviewer", memory_scope="owner"), type=MemoryType.USER,
                  key="drink", content="coffee", source="model_inferred")
    active = [e for e in store.list_all(session_key="owner") if not e.is_superseded]
    assert [e.content for e in active] == ["tea"]


@pytest.mark.asyncio
async def test_forgotten_fact_cannot_be_recreated_by_retried_extraction_after_reopen(tmp_path):
    svc, store = _svc(tmp_path)
    entry = store.add(MemoryEntry(key="drink", content="coffee", source_session="owner"))
    assert (await svc.remove(ActorContext(actor="admin", memory_scope="owner"), entry.id, override=True)).ok
    # A restarted worker may re-extract the old history; the barrier must live
    # with the authoritative data, rather than only in a coroutine-local flag.
    svc, store = _svc(tmp_path)
    for actor, source in [("reviewer", "model_inferred"), ("consolidation", "consolidated")]:
        result = await svc.add(ActorContext(actor=actor, memory_scope="owner"), type=MemoryType.USER,
                               key="drink", content="coffee", source=source)
        assert not result.ok
    assert store.list_all(session_key="owner") == []
    assert store.search_scored("coffee", session_key="owner") == []
    assert "coffee" not in store.get_snapshot(session_key="owner")


@pytest.mark.asyncio
async def test_forget_barrier_preserves_other_scopes_and_explicit_new_user_request(tmp_path):
    svc, store = _svc(tmp_path)
    entry = store.add(MemoryEntry(key="drink", content="coffee", source_session="owner"))
    await svc.remove(ActorContext(actor="admin", memory_scope="owner"), entry.id, override=True)
    other = await svc.add(ActorContext(actor="reviewer", memory_scope="other"), type=MemoryType.USER,
                          key="drink", content="coffee", source="model_inferred")
    explicit = await svc.add(ActorContext(actor="model", memory_scope="owner"), type=MemoryType.USER,
                             key="drink", content="tea", source="user_stated")
    assert other.ok and explicit.ok
    assert store.find_by_key("drink", session_key="owner").content == "tea"


@pytest.mark.asyncio
async def test_reviewer_cannot_replace_another_key_with_forgotten_content(tmp_path):
    svc, store = _svc(tmp_path)
    forgotten = store.add(MemoryEntry(key="drink", content="User prefers tea.", source_session="owner"))
    note = store.add(MemoryEntry(key="note", content="Unrelated note.", source_session="owner"))
    await svc.remove(ActorContext(actor="admin", memory_scope="owner"), forgotten.id, override=True)
    result = await svc.replace(ActorContext(actor="reviewer", memory_scope="owner"), note.id,
                               content="User prefers tea.", source="model_inferred")
    assert not result.ok and result.reason == "rejected_forgotten"
    assert store.get(note.id).content == "Unrelated note."


@pytest.mark.asyncio
async def test_reviewer_key_normalization_cannot_bypass_forget(tmp_path):
    svc, store = _svc(tmp_path)
    entry = store.add(MemoryEntry(key="drink", content="tea", source_session="owner"))
    await svc.remove(ActorContext(actor="admin", memory_scope="owner"), entry.id, override=True)
    result = await svc.add(ActorContext(actor="consolidation", memory_scope="owner"), type=MemoryType.USER,
                           key=" drink ", content="A paraphrase of the old tea preference", source="consolidated")
    assert not result.ok and result.reason == "rejected_forgotten"


@pytest.mark.asyncio
@pytest.mark.parametrize("actor", ["model", "reviewer", "consolidation"])
async def test_normalized_global_tag_cannot_bypass_environment_gate(tmp_path, actor):
    svc, store = _svc(tmp_path)
    result = await svc.add(ActorContext(actor=actor, memory_scope="owner"), type=MemoryType.USER,
                           key="private", content="private preference", tags=[" global "], source="model_inferred")
    assert not result.ok and result.reason == "rejected_env"
    assert store.list_all() == []
