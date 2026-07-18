"""S2: memory.enabled 作为真正总开关 —— disabled 时不注册 memory 工具、不注入任何长期记忆。

关闭前提下应满足两点：
1. discover_tools 拿不到 memory_store，不注册 memory 工具；
2. ContextStage 三分支统一短路，注入的 memory_context 只剩本轮 working memory，
   不含长期记忆引导语 / 快照内容。
"""

from __future__ import annotations

from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.tools import discover_tools
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.session.manager import Session


def test_disabled_registers_no_memory_tool(tmp_path):
    # memory_store 缺省为 None（模拟 disabled 时 loop 传 None），不得注册 memory 工具
    tools = discover_tools(config=Config(), workspace=tmp_path, bus=MessageBus())
    assert not any(t.name == "memory" for t in tools)


def test_enabled_registers_memory_tool(tmp_path):
    # 对照组：memory_store 存在时 memory 工具照常注册，确认开关是 memory_store 而非其他
    tools = discover_tools(
        config=Config(), workspace=tmp_path, bus=MessageBus(),
        memory_store=MagicMock(),
    )
    assert any(t.name == "memory" for t in tools)


def _make_stage(*, memory_enabled: bool, capture: dict):
    config = MagicMock()
    config.session.max_history_messages = 100
    config.memory.enabled = memory_enabled
    config.knowledge = MagicMock()
    config.knowledge.enabled = False

    memory = MagicMock()
    # 长期快照带明显标记，若被注入即可在 system prompt 中检出
    memory.get_snapshot = MagicMock(return_value="LONG_TERM_SNAPSHOT_MARKER")
    memory.get_snapshot_with_ids = MagicMock(
        return_value=("LONG_TERM_SNAPSHOT_MARKER", frozenset())
    )
    memory.search_scored = MagicMock(return_value=[])

    compressor = MagicMock()
    compressor.should_compress = MagicMock(return_value=False)

    def _capture_prompt(*, memory_context, skills_context, capabilities):
        capture["memory_context"] = memory_context
        return "SYS"

    context_builder = MagicMock()
    context_builder.build_system_prompt = MagicMock(side_effect=_capture_prompt)
    context_builder.build_messages = MagicMock(return_value=[
        {"role": "system", "content": "SYS"},
        {"role": "user", "content": "hi"},
    ])

    inference = MagicMock()
    inference.filter_tools = MagicMock(return_value=[])

    working = MagicMock()
    working.get_context = MagicMock(return_value="WORKING_MEMORY_ONLY")
    working_memories = OrderedDict()
    working_memories["cli:c1"] = working

    return ContextStage(
        config=config,
        sessions=AsyncMock(),
        memory=memory,
        compressor=compressor,
        context_builder=context_builder,
        skill_store=None,
        knowledge=None,
        hybrid_retriever=None,
        planner=None,
        inference=inference,
        working_memories=working_memories,
        memory_snapshots=OrderedDict(),
        snapshot_enabled=True,
        tool_definitions_fn=lambda: [],
        memory_enabled=memory_enabled,
    )


@pytest.mark.asyncio
async def test_disabled_injects_only_working_memory():
    capture: dict = {}
    stage = _make_stage(memory_enabled=False, capture=capture)
    event = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="hi")
    await stage.build(
        event, Session(key="cli:c1"),
        publish_response=False, trace_id="t", stream_publisher=None, intro_text="",
    )
    mem_ctx = capture["memory_context"]
    # disabled：只保留本轮 working memory，不含长期快照 / 记忆引导语
    assert mem_ctx == "WORKING_MEMORY_ONLY"
    assert "LONG_TERM_SNAPSHOT_MARKER" not in mem_ctx
    assert "persistent memory" not in mem_ctx


@pytest.mark.asyncio
async def test_enabled_injects_long_term_memory():
    # 对照组：enabled 时长期快照 / 引导语照旧注入
    capture: dict = {}
    stage = _make_stage(memory_enabled=True, capture=capture)
    event = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="hi")
    await stage.build(
        event, Session(key="cli:c1"),
        publish_response=False, trace_id="t", stream_publisher=None, intro_text="",
    )
    mem_ctx = capture["memory_context"]
    assert "LONG_TERM_SNAPSHOT_MARKER" in mem_ctx
