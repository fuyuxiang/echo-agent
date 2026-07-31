"""ContextStage 的关键词兜底召回路径必须走 RETRIEVAL 资格过滤。

止血层 S1 收口：无向量索引兜底 / Hybrid 超时兜底 / sync 无 retriever 兜底
三条内联 search_scored 调用，都直接喂进 prompt，属于「该显示的召回」路径，
必须过滤掉 superseded/archived/unresolved 条目，不能漏进 LLM。
"""
import asyncio
from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.bus.events import InboundEvent
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.session.manager import Session


def _store_with_superseded(tmp_path):
    """构造含一条 live + 一条 superseded 的 store，两条都命中关键词「北京」。"""
    s = MemoryStore(memory_dir=tmp_path / "mem")
    live = s.add(MemoryEntry(type=MemoryType.USER, key="city", content="现居北京朝阳", source="user_stated"))
    old = s.add(MemoryEntry(type=MemoryType.USER, key="city_old", content="旧地址北京海淀", source="user_stated"))
    s.mark_superseded(old.id, live.id)
    return s, live, old


def _make_stage(memory, *, hybrid_retriever=None, timeout=0.8, on_miss="degrade"):
    """构造一个只关心召回兜底路径的 ContextStage，其余依赖 mock 掉。"""
    config = MagicMock()
    config.session.max_history_messages = 100
    config.memory.enabled = True
    config.knowledge.enabled = False
    inference = MagicMock()
    inference.filter_tools = MagicMock(return_value=[])
    context_builder = MagicMock()
    context_builder.build_system_prompt = MagicMock(return_value="sys")
    context_builder.build_messages = MagicMock(return_value=[{"role": "user", "content": "北京"}])
    context_builder.resolve_inbound_media = AsyncMock(return_value=None)
    compressor = MagicMock()
    compressor.should_compress = MagicMock(return_value=False)
    sessions = AsyncMock()
    return ContextStage(
        config=config,
        sessions=sessions,
        memory=memory,
        compressor=compressor,
        context_builder=context_builder,
        skill_store=None,
        knowledge=None,
        hybrid_retriever=hybrid_retriever,
        planner=None,
        inference=inference,
        working_memories=OrderedDict(),
        memory_snapshots=OrderedDict(),
        snapshot_enabled=False,
        tool_definitions_fn=lambda channel=None: [],
        retrieval_on_miss=on_miss,
        retrieval_miss_timeout=timeout,
    )


@pytest.mark.asyncio
async def test_no_retriever_fallback_hides_superseded(tmp_path):
    """无向量索引兜底（timeout>0 且无 hybrid_retriever）不得召回 superseded。"""
    memory, live, old = _store_with_superseded(tmp_path)
    stage = _make_stage(memory, hybrid_retriever=None, timeout=0.8)
    event = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="北京")
    scored = await stage._bounded_retrieve(event, publish_response=False)
    ids = [e.id for e, _ in (scored or [])]
    assert old.id not in ids, "superseded 条目漏进无向量索引兜底召回"
    assert live.id in ids, "live 条目应正常召回"


@pytest.mark.asyncio
async def test_timeout_fallback_hides_superseded(tmp_path):
    """Hybrid retrieve 超时后的关键词兜底不得召回 superseded。"""
    memory, live, old = _store_with_superseded(tmp_path)

    async def _slow_retrieve(*args, **kwargs):
        await asyncio.sleep(1.0)
        return []

    retriever = MagicMock()
    retriever.retrieve = _slow_retrieve
    stage = _make_stage(memory, hybrid_retriever=retriever, timeout=0.01)
    event = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="北京")
    scored = await stage._bounded_retrieve(event, publish_response=False)
    ids = [e.id for e, _ in (scored or [])]
    assert old.id not in ids, "superseded 条目漏进超时兜底召回"
    assert live.id in ids, "live 条目应正常召回"


@pytest.mark.asyncio
async def test_sync_no_retriever_injection_hides_superseded(tmp_path):
    """retrieval_on_miss==sync 但无 hybrid_retriever 时，注入内容不得含 superseded。"""
    memory, live, old = _store_with_superseded(tmp_path)
    stage = _make_stage(memory, hybrid_retriever=None, on_miss="sync")
    event = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="北京")
    session = Session(key="cli:c1")
    ctx = await stage.build(
        event, session,
        publish_response=False, trace_id="t1", stream_publisher=None, intro_text="",
    )
    assert "海淀" not in ctx.retrieval, "superseded 原文漏进 sync 兜底注入"
    assert "朝阳" in ctx.retrieval, "live 条目应正常注入"
