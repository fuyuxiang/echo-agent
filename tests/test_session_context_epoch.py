import asyncio
from collections import OrderedDict
from unittest.mock import MagicMock

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.agent.pipeline.context_stage import (
    contextual_retrieval_query,
    planning_context,
)
from echo_agent.session.context_epoch import (
    belongs_to_session,
    conversation_context_key,
)
from echo_agent.session.manager import Session


def test_context_epoch_uses_persisted_reset_count():
    session = Session(key="cli:local")
    assert conversation_context_key(session.key, session) == "cli:local"
    session.metadata["reset_count"] = 3
    assert conversation_context_key(session.key, session) == "cli:local::epoch:3"
    assert belongs_to_session("cli:local::epoch:2", "cli:local")
    assert not belongs_to_session("cli:local-extra::epoch:2", "cli:local")


def test_deictic_query_is_bound_to_immediate_conversation():
    history = [
        {"role": "user", "content": "帮我审一下 echo-agent.yaml"},
        {"role": "assistant", "content": "建议把 apiKey 改为 api_key，并改用 api_key_env。"},
    ]
    query = contextual_retrieval_query("帮我逐项执行上述优化", history)
    assert "api_key_env" in query
    assert "逐项执行上述优化" in query
    # A self-contained task does not pay for or get polluted by old history.
    assert contextual_retrieval_query("修复 login.py 的报错", history) == "修复 login.py 的报错"


def test_planner_context_labels_recent_chat_as_authoritative():
    context = planning_context(
        [{"role": "assistant", "content": "这一轮需要修改 yaml"}],
        "old checklist: rewrite README",
    )
    assert "authoritative" in context
    assert "修改 yaml" in context
    assert "may be stale" in context


@pytest.mark.asyncio
async def test_reset_clears_only_reset_bounded_process_state():
    loop = AgentLoop.__new__(AgentLoop)
    loop._state_lock = asyncio.Lock()
    loop._working_memories = OrderedDict([
        ("cli:local", object()),
        ("cli:local::epoch:1", object()),
        ("cli:other", object()),
    ])
    loop._memory_snapshots = OrderedDict(loop._working_memories)
    loop._memory_snapshot_ids = OrderedDict(loop._working_memories)
    loop._retrieval_cache = OrderedDict(loop._working_memories)
    loop._memory_snapshot_meta = {key: ("scope", 1) for key in loop._working_memories}
    loop.compressor = MagicMock()
    loop._response_stage = MagicMock()
    loop.clarify = MagicMock()
    loop.approval = MagicMock()
    loop.interrupt = MagicMock()

    await loop.reset_session_state("cli:local")

    for cache in (
        loop._working_memories, loop._memory_snapshots,
        loop._memory_snapshot_ids, loop._retrieval_cache,
        loop._memory_snapshot_meta,
    ):
        assert list(cache) == ["cli:other"]
    loop.compressor.on_session_reset.assert_called_once_with("cli:local")
    loop.clarify.cancel_session.assert_called_once_with("cli:local")
    loop.approval.cancel_session.assert_called_once()
