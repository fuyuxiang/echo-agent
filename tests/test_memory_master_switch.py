"""S2: memory.enabled 作为真正总开关 —— disabled 时不注册 memory 工具、不注入任何长期记忆、
不调度 consolidation / Reviewer / working 写回、REST 端点返回 409。

关闭前提下应满足：
1. discover_tools 拿不到 memory_store，不注册 memory 工具；
2. ContextStage 三分支统一短路，注入的 memory_context 只剩本轮 working memory，
   不含长期记忆引导语 / 快照内容；
3. ResponseStage 不 flush、不调度 consolidation、不派 Reviewer、不写回 working memory；
4. 不变量 4：enabled=False 处理多轮消息后，记忆目录文件 mtime 与 SQLite 行数不变。
"""

from __future__ import annotations

import asyncio
from collections import OrderedDict
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.response_stage import ResponseStage
from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.agent.tools import discover_tools
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import Config
from echo_agent.models.provider import LLMProvider, LLMResponse
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
        tool_definitions_fn=lambda channel=None: [],
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


def _make_response_stage(*, memory_enabled: bool, spawn_calls: list, working_adds: list):
    config = MagicMock()
    config.memory.enabled = memory_enabled

    memory = MagicMock()
    memory.has_pending_embeds = MagicMock(return_value=True)

    consolidation = MagicMock()
    consolidator = MagicMock()
    consolidator.should_consolidate = MagicMock(return_value=True)
    consolidation._consolidator = consolidator
    consolidation.schedule = AsyncMock()

    def _spawn(coro_or_fn, tier=None):
        spawn_calls.append((coro_or_fn, tier))
        # 关掉真正协程，避免 "coroutine was never awaited" 警告
        if asyncio.iscoroutine(coro_or_fn):
            coro_or_fn.close()

    working = MagicMock()
    working.add = MagicMock(side_effect=lambda entry: working_adds.append(entry))
    working_memories = OrderedDict()
    working_memories["cli:c1"] = working

    return ResponseStage(
        config=config,
        sessions=AsyncMock(),
        memory=memory,
        provider=MagicMock(),
        consolidation_worker=consolidation,
        default_model="stub",
        spawn_fn=_spawn,
        clear_memory_snapshot_fn=AsyncMock(),
        working_memories=working_memories,
        memory_enabled=memory_enabled,
    )


def _make_ctx_result():
    event = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="hi")
    session = Session(key="cli:c1")
    # message_count 抬高，让 should_consolidate 有机会返回 True（此处已被 mock 强制 True）
    ctx = PipelineContext(
        event=event, session=session, trace_id="t", publish_response=False,
        messages=[{"role": "user", "content": "hi"}],
    )
    result = InferenceResult(
        response_text="ok", total_tool_calls=1,
        should_review_skills=False, should_review_memory=True,
    )
    return ctx, result


@pytest.mark.asyncio
async def test_disabled_response_stage_schedules_no_memory_work():
    # disabled：flush / consolidation / memory review / working 写回全部短路
    spawn_calls: list = []
    working_adds: list = []
    stage = _make_response_stage(
        memory_enabled=False, spawn_calls=spawn_calls, working_adds=working_adds
    )
    ctx, result = _make_ctx_result()
    await stage.finalize(ctx, result)

    stage._consolidation.schedule.assert_not_called()
    assert spawn_calls == []  # 无 flush、无 memory review 派发
    assert working_adds == []  # 无 working memory 写回


@pytest.mark.asyncio
async def test_enabled_response_stage_schedules_memory_work():
    # 对照组：enabled 时 flush / consolidation / memory review / working 写回照常
    spawn_calls: list = []
    working_adds: list = []
    stage = _make_response_stage(
        memory_enabled=True, spawn_calls=spawn_calls, working_adds=working_adds
    )
    ctx, result = _make_ctx_result()
    await stage.finalize(ctx, result)

    stage._consolidation.schedule.assert_called_once()
    assert spawn_calls, "enabled 时应至少派发 flush / memory review"
    assert working_adds, "enabled 时应写回 working memory"


class _StubProvider(LLMProvider):
    def __init__(self):
        super().__init__()
        self._resp = LLMResponse(content="ok", finish_reason="stop")

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return self._resp

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        if self._resp.content and on_delta:
            r = on_delta(self._resp.content)
            if asyncio.iscoroutine(r):
                await r
        return self._resp

    def get_default_model(self):
        return "stub"


def _dir_snapshot(root: Path) -> dict[str, float]:
    if not root.exists():
        return {}
    return {
        str(p): p.stat().st_mtime
        for p in root.rglob("*") if p.is_file()
    }


@pytest.mark.asyncio
async def test_disabled_no_side_effects_after_messages(tmp_path):
    # 不变量 4：memory.enabled=False 处理多轮消息后，记忆目录文件 mtime 与
    # SQLite memories/episodes 行数不变。
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config

    config = load_config(overrides={
        "workspace": str(tmp_path),
        "memory": {"enabled": False},
    })
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_StubProvider(), workspace=tmp_path)

    memory_dir = tmp_path / config.storage.memory_dir

    def _sqlite_counts() -> dict[str, int]:
        import sqlite3
        counts: dict[str, int] = {}
        db = tmp_path / config.storage.database_path
        if not db.exists():
            return counts
        conn = sqlite3.connect(str(db))
        try:
            for tbl in ("memories", "episodes"):
                try:
                    counts[tbl] = conn.execute(f"SELECT COUNT(*) FROM {tbl}").fetchone()[0]
                except sqlite3.OperationalError:
                    counts[tbl] = 0
        finally:
            conn.close()
        return counts

    # 先跑一轮，让懒创建的目录/表落地，再取基线快照
    ev0 = InboundEvent.text_message(channel="cli", sender_id="u", chat_id="c1", text="warmup")
    await loop._process_event(ev0, "trace-0")

    before_files = _dir_snapshot(memory_dir)
    before_counts = _sqlite_counts()

    for i in range(3):
        ev = InboundEvent.text_message(
            channel="cli", sender_id="u", chat_id="c1", text=f"我叫小明，住在上海，第{i}条",
        )
        await loop._process_event(ev, f"trace-{i}")

    after_files = _dir_snapshot(memory_dir)
    after_counts = _sqlite_counts()

    assert after_files == before_files, "disabled 时记忆目录文件不应新增或被改写"
    assert after_counts == before_counts, "disabled 时 SQLite 记忆行数不应变化"


def _make_memory_api(*, enabled: bool):
    from echo_agent.gateway.api.memory import MemoryAPI

    server = MagicMock()
    server._agent_loop.config.memory.enabled = enabled
    # guard 放行(返回 None),隔离 disabled 分支
    server._require_admin_token = MagicMock(return_value=None)
    return MemoryAPI(server)


@pytest.mark.asyncio
async def test_rest_endpoints_return_409_when_disabled():
    api = _make_memory_api(enabled=False)
    request = MagicMock()
    request.query = {}
    for handler in (api.list_entries, api.stats, api.get_entry,
                    api.update_entry, api.delete_entry, api.search):
        resp = await handler(request)
        assert resp.status == 409, f"{handler.__name__} disabled 时应返回 409"


@pytest.mark.asyncio
async def test_rest_stats_not_409_when_enabled():
    # 对照组:enabled 时 disabled 分支不触发(此处 store 被 mock,只验证不是 409)
    api = _make_memory_api(enabled=True)
    api._server._agent_loop.memory.list_all = MagicMock(return_value=[])
    request = MagicMock()
    request.query = {}
    resp = await api.stats(request)
    assert resp.status != 409
