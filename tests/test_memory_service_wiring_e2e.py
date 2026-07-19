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
        await loop.stop()


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
        await loop.stop()


def test_store_service_only_soft_warns_on_direct_write(tmp_path, caplog):
    """service_only=True 的 store:非 service 路径直写 add 告警;service 通道写不告警。"""
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryEntry, MemoryType

    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", service_only=True)

    import logging
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
