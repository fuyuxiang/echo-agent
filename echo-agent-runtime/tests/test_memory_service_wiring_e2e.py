"""R1 Task8: loop 统一装配 MemoryService 单例的接线验收。

前 7 任务各写入口就近 new 了独立 MemoryService,失效/审计各自为政。本用例锁定
收口后的不变量:loop 上存在唯一 self._memory_service,且 6 类写者(MemoryTool /
Reviewer(ResponseStage) / SemanticManager / ArchivalManager / ReflectionEngine /
ContradictionDetector)持有的是同一实例。另验证 store 的 _service_only 软约束:
service 通道写不告警,外部直写告警。
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider


def _make_loop(tmp_path):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.storage.sqlite import SQLiteBackend

    config = load_config(overrides={"workspace": str(tmp_path)})
    config.memory.enabled = True
    config.memory.vector_enabled = False  # 关向量走关键词模式,免加载本地模型
    config.memory.contradiction_detection = True
    config.memory.reflection_enabled = True
    config.knowledge.enabled = False
    config.planning.enabled = False
    config.multi_agent.enabled = False
    config.observability.otel_enabled = False
    config.observability.trace_enabled = False

    provider = MagicMock(spec=LLMProvider)
    provider.supports_embed = MagicMock(return_value=False)
    provider.get_default_model = MagicMock(return_value="stub")
    provider.chat_with_retry = AsyncMock()

    storage = SQLiteBackend(tmp_path / "wiring.db")
    loop = AgentLoop(
        bus=MessageBus(), config=config, provider=provider,
        workspace=tmp_path, storage=storage,
    )
    return loop


@pytest.mark.asyncio
async def test_all_writers_share_single_service(tmp_path):
    from echo_agent.memory.service import MemoryService

    loop = _make_loop(tmp_path)
    # start() 内 _resolve_embed_and_index→_wire_vector_consumers 构造 detector/reflection。
    await loop.start()
    try:
        svc = getattr(loop, "_memory_service", None)
        assert isinstance(svc, MemoryService), "loop 未构造 _memory_service 单例"
        # 单例底层 store 就是 loop.memory
        assert svc.store is loop.memory

        # ① MemoryTool
        mem_tool = loop.tools.get("memory")
        assert mem_tool is not None
        assert mem_tool._service is svc, "MemoryTool 未注入单例"

        # ② Reviewer 走 ResponseStage
        assert loop._response_stage._memory_service is svc, "ResponseStage 未注入单例"

        # ③ SemanticManager / ④ ArchivalManager
        assert loop.consolidator._semantic_manager._service is svc, "SemanticManager 未注入单例"
        assert loop.consolidator._archival_manager._service is svc, "ArchivalManager 未注入单例"

        # ⑤ ReflectionEngine
        assert loop.consolidator._reflection_engine._service is svc, "ReflectionEngine 未注入单例"

        # ⑥ ContradictionDetector
        assert loop._contradiction_detector._service is svc, "ContradictionDetector 未注入单例"
    finally:
        try:
            await loop.stop()
        finally:
            await loop._storage.close()


@pytest.mark.asyncio
async def test_reviewer_service_shared_via_response_stage(tmp_path):
    """ResponseStage 后台 memory review 用注入的单例,而非每次 new 一个。"""
    loop = _make_loop(tmp_path)
    await loop.start()
    try:
        # 反射 review 构造的 MemoryReviewer 应持有 loop 单例
        from echo_agent.memory.reviewer import MemoryReviewer

        captured = {}
        orig_init = MemoryReviewer.__init__

        def _spy(self, *a, **kw):
            captured["service"] = kw.get("service")
            orig_init(self, *a, **kw)

        MemoryReviewer.__init__ = _spy
        try:
            await loop._response_stage._background_memory_review(
                [{"role": "user", "content": "hi"}], "sess", "scope1"
            )
        finally:
            MemoryReviewer.__init__ = orig_init
        assert captured.get("service") is loop._memory_service
    finally:
        try:
            await loop.stop()
        finally:
            await loop._storage.close()


def test_store_service_only_soft_warns_on_direct_write(tmp_path, caplog):
    """service_only=True 的 store:非 service 路径直写 add 告警;service 通道写不告警。"""
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryEntry, MemoryType

    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", service_only=True)

    from loguru import logger as _logger

    records: list[str] = []
    sink_id = _logger.add(lambda m: records.append(m), level="WARNING")
    try:
        # 外部直写 → 告警
        store.add(MemoryEntry(type=MemoryType.USER, key="k", content="v", source_session="s"))
        assert any("service" in r.lower() for r in records), "直写未告警"

        # service 通道写 → 不告警
        records.clear()
        with store.service_write():
            store.add(MemoryEntry(type=MemoryType.USER, key="k2", content="v2", source_session="s"))
        assert not any("service" in r.lower() for r in records), "service 通道写不应告警"
    finally:
        _logger.remove(sink_id)


# ── R1 核心收益:写后立即生效(失效闭环)的端到端断言 ────────────────────────
#
# R1 把 6 类写入口收敛到 MemoryService 单例,核心收益是补全失效漏口:任一写口
# 写 scope X 后,共享 X 的其它 session 下一轮读能立即读到新值。此前 Reviewer/
# reflection/tiers/detector 直接写 store 不触发失效,跨通道共享 scope 的快照会读到
# 旧值。以下用例走真实 MemoryStore + MemoryService + 真实的 bump-scope-version 失效
# 回调(取自 AgentLoop 的 _invalidate_memory_caches / put_memory_snapshot),断言:
#   ① 写后失效回调收到正确的 (scope, global_scope);
#   ② 模拟"另一 session 共享 scope X"的读路径(与 ContextStage 的 snapshot 版本校验
#      逻辑一致),写前缓存的快照因 scope 版本被 bump 而失效,下一轮读到新值。
# 覆盖两类写口(model 工具写 add + maintenance 裁决写 maintenance_update),证明失效
# 不是只挂在某一条路径上。


def _make_cache_host(tmp_path):
    """构造一个最小 AgentLoop 替身:复用其真实的失效/快照缓存方法与状态,
    不跑完整 start() 装配(过重)。invalidate_fn 绑到真实的 _invalidate_memory_caches,
    读路径复用 _scope_version + put_memory_snapshot,与 ContextStage 校验语义一致。"""
    import asyncio
    from collections import OrderedDict
    from echo_agent.agent.loop import AgentLoop

    host = AgentLoop.__new__(AgentLoop)
    host._state_lock = asyncio.Lock()
    host._memory_snapshots = OrderedDict()
    host._memory_snapshot_ids = OrderedDict()
    host._memory_snapshot_meta = {}
    host._retrieval_cache = OrderedDict()
    host._scope_versions = {}
    host._max_cached_sessions = 200
    return host


async def _read_snapshot(host, store, session_key: str, memory_scope: str):
    """模拟 ContextStage 的 snapshot 读路径:按 (scope, 当前 scope 版本) 校验缓存,
    命中则复用,否则从 store 重建并按当前版本回写缓存。返回 (snapshot, from_cache)。"""
    cur_ver = host._scope_version(memory_scope)
    meta = host._memory_snapshot_meta.get(session_key)
    snapshot_valid = (
        session_key in host._memory_snapshots
        and meta is not None
        and meta == (memory_scope, cur_ver)
    )
    if snapshot_valid:
        return host._memory_snapshots[session_key], True
    snapshot, ids = store.get_snapshot_with_ids(session_key=memory_scope)
    await host.put_memory_snapshot(session_key, snapshot, ids, memory_scope, cur_ver)
    return snapshot, False


@pytest.mark.asyncio
async def test_model_add_invalidates_shared_scope_snapshot(tmp_path):
    """model 工具写 add 到 scope X 后:失效回调收到 (X, False),且另一 session
    共享 X 的缓存快照失效、下一轮读到新写入的内容。"""
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.service import MemoryService, ActorContext
    from echo_agent.memory.types import MemoryType

    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", service_only=True)
    host = _make_cache_host(tmp_path)

    calls: list[tuple[str, bool]] = []

    async def _inval(scope: str, global_scope: bool):
        calls.append((scope, global_scope))
        await host._invalidate_memory_caches(scope, global_scope)

    svc = MemoryService(store, invalidate_fn=_inval)

    scope = "scope-shared"
    # session_writer 与 session_reader 是两个不同 session,但共享同一 memory_scope。
    reader = "session-reader"

    # ① reader 先读一轮:此时 scope 无内容,快照为空,缓存 (scope, 版本0)。
    snap0, from_cache0 = await _read_snapshot(host, store, reader, scope)
    assert from_cache0 is False
    assert "上海人" not in snap0

    # ② 另一 session 经 model 工具写口写入 scope X 的新事实。
    r = await svc.add(
        ActorContext(actor="model", session_key="session-writer", memory_scope=scope),
        type=MemoryType.USER, key="hometown", content="用户是上海人",
        importance=0.6, source="user_stated",
    )
    assert r.ok is True

    # 失效回调收到正确 (scope, global_scope=False):USER 写是 per-scope 失效。
    assert calls == [(scope, False)]
    # scope 版本被 bump(0→1),使旧版本快照失效。
    assert host._scope_version(scope) == 1

    # ③ reader 下一轮读:缓存 meta 停留在版本0,与当前版本1 不符 → 未命中,
    #    从 store 重建并读到新写入的内容。这正是 R1 补全的"写后立即生效"。
    snap1, from_cache1 = await _read_snapshot(host, store, reader, scope)
    assert from_cache1 is False, "scope 被写后共享快照应失效,而非命中旧缓存"
    assert "上海人" in snap1


@pytest.mark.asyncio
async def test_maintenance_update_invalidates_shared_scope_snapshot(tmp_path):
    """maintenance 裁决写 maintenance_update 后:失效回调收到 (X, False),
    另一 session 共享 X 的缓存快照失效、下一轮读到更新后的内容。覆盖第二类写口。"""
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.service import MemoryService, ActorContext
    from echo_agent.memory.types import MemoryEntry, MemoryType

    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", service_only=True)
    host = _make_cache_host(tmp_path)

    calls: list[tuple[str, bool]] = []

    async def _inval(scope: str, global_scope: bool):
        calls.append((scope, global_scope))
        await host._invalidate_memory_caches(scope, global_scope)

    svc = MemoryService(store, invalidate_fn=_inval)

    scope = "scope-shared"
    reader = "session-reader"

    # 先在 scope 内种一条高优先级条目(importance 足以进快照)。
    with store.service_write():
        e = store.add(MemoryEntry(
            type=MemoryType.USER, key="pref", content="喜欢喝美式",
            importance=0.6, source="user_stated", source_session=scope,
        ))

    # ① reader 读一轮:读到旧内容并缓存 (scope, 版本0)。
    snap0, from_cache0 = await _read_snapshot(host, store, reader, scope)
    assert from_cache0 is False
    assert "美式" in snap0 and "拿铁" not in snap0

    # ② maintenance 走 maintenance_update 改内容(精简写序,跳过 provenance,仍失效)。
    r = await svc.maintenance_update(
        ActorContext(actor="maintenance", session_key=scope, memory_scope=scope),
        e.id, content="改喝拿铁了",
    )
    assert r.ok is True

    # 失效回调收到 (scope, False);scope 版本被 bump。
    assert calls == [(scope, False)]
    assert host._scope_version(scope) == 1

    # ③ reader 下一轮读:旧缓存失效,读到更新后的内容。
    snap1, from_cache1 = await _read_snapshot(host, store, reader, scope)
    assert from_cache1 is False, "maintenance 写后共享快照应失效"
    assert "拿铁" in snap1
