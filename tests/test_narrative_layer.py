from collections import OrderedDict
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.bus.events import InboundEvent
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.tiers import EpisodicManager
from echo_agent.session.manager import Session
from echo_agent.storage.sqlite import SQLiteBackend


def test_snapshot_injects_episode_summaries_as_narrative(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    snap, _ = s.get_snapshot_with_ids(
        session_key="x",
        episode_summaries=["用户先在北京、因工作搬到上海", "讨论了部署方案"],
    )
    assert "## Recent Context" in snap
    assert "因工作搬到上海" in snap and "部署方案" in snap


def test_snapshot_no_narrative_when_empty(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    snap, _ = s.get_snapshot_with_ids(session_key="x", episode_summaries=None)
    assert "## Recent Context" not in snap  # 无 summary 不注入空段


@pytest_asyncio.fixture
async def storage(tmp_path):
    backend = SQLiteBackend(tmp_path / "narrative.db")
    await backend.initialize()
    yield backend
    await backend.close()


@pytest.mark.asyncio
async def test_narrative_prefetch_uses_session_key_not_memory_scope(storage, tmp_path):
    """owner 私聊回归：episode 按真实 session_key 存储，而 event.memory_scope
    因 cross_channel_owner 归一为 "owner"（≠session_key）。叙事层预取必须用
    session_key 查询，否则 get_session_episodes("owner") 恒空、## Recent Context
    对 owner 永久失效。修复前用 memory_scope 查恒空（红），修复后用 session_key 命中（绿）。"""
    real_session_key = "telegram:bob"
    episodic = EpisodicManager(storage)
    await episodic.create_episode(real_session_key, [], "上周聊了搬家到上海的事")

    # event.session_key == "telegram:bob"，但 memory_scope 归一到 "owner"
    event = InboundEvent.text_message(
        channel="telegram", sender_id="bob", chat_id="bob", text="继续",
    )
    event.memory_scope = "owner"
    assert event.session_key == real_session_key != event.memory_scope

    memory = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")

    capture: dict = {}

    def _capture_prompt(*, memory_context, skills_context, capabilities):
        capture["memory_context"] = memory_context
        return "SYS"

    config = MagicMock()
    config.session.max_history_messages = 100
    config.memory.enabled = True
    config.knowledge = MagicMock()
    config.knowledge.enabled = False

    compressor = MagicMock()
    compressor.should_compress = MagicMock(return_value=False)

    context_builder = MagicMock()
    context_builder.build_system_prompt = MagicMock(side_effect=_capture_prompt)
    context_builder.build_messages = MagicMock(return_value=[
        {"role": "system", "content": "SYS"},
        {"role": "user", "content": "继续"},
    ])

    inference = MagicMock()
    inference.filter_tools = MagicMock(return_value=[])

    stage = ContextStage(
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
        working_memories=OrderedDict(),
        memory_snapshots=OrderedDict(),
        snapshot_enabled=True,
        tool_definitions_fn=lambda: [],
        episodic=episodic,
        narrative_episode_count=3,
    )

    await stage.build(
        event, Session(key=real_session_key),
        publish_response=False, trace_id="t", stream_publisher=None, intro_text="",
    )

    # 叙事层必须命中真实 session_key 下的 episode summary
    assert "## Recent Context" in capture["memory_context"]
    assert "搬家到上海" in capture["memory_context"]

