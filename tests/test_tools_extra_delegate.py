"""Contract tests for DelegateTool and SpawnTool."""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.delegate import (
    DelegateTool,
    SpawnTool,
    WORKER_BLOCKED_TOOLS,
)
from echo_agent.agent.multi_agent.models import WorkerProfile, WorkerResult


def _ctx(**kwargs) -> ToolExecutionContext:
    defaults = {"session_key": "cli:c1", "user_id": "u1", "trace_id": "t1"}
    defaults.update(kwargs)
    return ToolExecutionContext(**defaults)


def _make_delegate(**overrides) -> DelegateTool:
    registry = MagicMock()
    registry.ready_tool_names = {"search", "read_file", "delegate_task", "clarify"}
    registry.get_ready_definitions.return_value = [
        {"type": "function", "function": {"name": "search"}},
        {"type": "function", "function": {"name": "read_file"}},
    ]
    worker_registry = MagicMock()
    defaults = dict(
        provider=MagicMock(),
        model_router=None,
        tool_registry=registry,
        worker_registry=worker_registry,
        approval_gate=MagicMock(),
        credentials=MagicMock(),
        audit_path=None,
        max_depth=3,
        max_parallel_workers=4,
        max_worker_iterations=12,
        default_model="m",
    )
    defaults.update(overrides)
    return DelegateTool(**defaults)


class TestDelegateNormalizeAndResolve:
    def test_normalize_tasks_array(self):
        tool = _make_delegate()
        tasks = tool._normalize_tasks({"tasks": [{"goal": "a"}, {"goal": "b"}]})
        assert len(tasks) == 2

    def test_normalize_single_goal(self):
        tool = _make_delegate()
        tasks = tool._normalize_tasks({"goal": "do thing", "tools": ["search"]})
        assert len(tasks) == 1
        assert tasks[0]["goal"] == "do thing"

    def test_normalize_empty(self):
        tool = _make_delegate()
        assert tool._normalize_tasks({}) == []

    def test_resolve_worker_tools_requested_intersect(self):
        tool = _make_delegate()
        available = {"search", "read_file"}
        result = tool._resolve_worker_tools({"tools": ["search", "nonexistent"]}, available)
        assert result == {"search"}

    def test_resolve_worker_tools_profile_defaults(self):
        worker_registry = MagicMock()
        worker_registry.get.return_value = WorkerProfile(
            id="coder", name="Coder", default_tools=("read_file",)
        )
        tool = _make_delegate(worker_registry=worker_registry)
        result = tool._resolve_worker_tools(
            {"worker_profile": "coder"}, {"search", "read_file"}
        )
        assert result == {"read_file"}

    def test_resolve_worker_tools_all_when_unspecified(self):
        tool = _make_delegate()
        available = {"search", "read_file"}
        assert tool._resolve_worker_tools({}, available) == available

    def test_get_depth_no_ctx(self):
        tool = _make_delegate()
        assert tool._get_depth(None) == 0

    def test_get_depth_worker_parent(self):
        tool = _make_delegate()
        ctx = _ctx(parent_execution_id="worker:abc:2")
        assert tool._get_depth(ctx) == 3

    def test_get_depth_worker_parent_no_index(self):
        tool = _make_delegate()
        ctx = _ctx(parent_execution_id="worker:abc")
        assert tool._get_depth(ctx) == 1

    def test_execution_mode(self):
        tool = _make_delegate()
        assert tool.execution_mode({}) == "side_effect"

    def test_blocked_tools_constant(self):
        assert "delegate_task" in WORKER_BLOCKED_TOOLS
        assert "clarify" in WORKER_BLOCKED_TOOLS


class TestDelegateFormatResults:
    def test_format_results_includes_status_and_duration(self):
        results = [
            WorkerResult(task_index=0, status="completed", output="done",
                         iterations=3, tool_calls=2, duration_seconds=1.5),
            WorkerResult(task_index=1, status="failed", error="boom",
                         duration_seconds=0.5),
        ]
        out = DelegateTool._format_results(results, 2.0)
        assert "Worker 0" in out
        assert "status=completed" in out
        assert "done" in out
        assert "Error: boom" in out
        assert "Total duration: 2.0s" in out


class TestDelegateExecute:
    @pytest.mark.asyncio
    async def test_depth_limit_reached(self):
        tool = _make_delegate(max_depth=1)
        ctx = _ctx(parent_execution_id="worker:abc:0")  # depth 1
        result = await tool.execute({"goal": "x"}, ctx)
        assert result.success is False
        assert "depth limit" in result.error.lower()

    @pytest.mark.asyncio
    async def test_no_tasks(self):
        tool = _make_delegate()
        result = await tool.execute({}, _ctx())
        assert result.success is False
        assert "No tasks specified" in result.error

    @pytest.mark.asyncio
    async def test_execute_happy_path(self):
        tool = _make_delegate()
        # Patch the internal executor's run to avoid real LLM loop.
        tool._executor.run = AsyncMock(return_value=WorkerResult(
            task_index=0, status="completed", output="worker done",
            iterations=1, tool_calls=0, duration_seconds=0.1,
        ))
        result = await tool.execute({"goal": "research X", "tools": ["search"]}, _ctx())
        assert result.success is True
        assert "worker done" in result.output
        assert result.metadata["worker_count"] == 1

    @pytest.mark.asyncio
    async def test_execute_worker_exception_marked_failed(self):
        tool = _make_delegate()
        tool._executor.run = AsyncMock(side_effect=RuntimeError("kaboom"))
        result = await tool.execute({"goal": "research X", "tools": ["search"]}, _ctx())
        assert result.success is False
        assert "kaboom" in result.output

    @pytest.mark.asyncio
    async def test_execute_truncates_to_max_parallel(self):
        tool = _make_delegate(max_parallel_workers=2)
        tool._executor.run = AsyncMock(return_value=WorkerResult(
            task_index=0, status="completed", output="ok",
        ))
        tasks = [{"goal": f"t{i}"} for i in range(5)]
        result = await tool.execute({"tasks": tasks}, _ctx())
        assert result.metadata["worker_count"] == 2


class TestSpawnTool:
    @pytest.mark.asyncio
    async def test_spawn_returns_task_id(self):
        provider = MagicMock()
        # _run_background awaits chat_with_retry; make it return quickly.
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="bg result")
        )
        tool = SpawnTool(provider=provider, bus=None)
        result = await tool.execute({"task": "do background work"}, _ctx())
        assert result.success is True
        assert result.metadata["task_id"].startswith("bg_")
        # Let the background task finish to avoid pending-task warnings.
        pending = list(tool._tasks.values())
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)

    @pytest.mark.asyncio
    async def test_spawn_publishes_correct_channel(self):
        """Background task completion must publish InboundEvent with the
        channel derived from session_key, not hardcoded 'system'."""
        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="weather result")
        )
        bus = MagicMock()
        bus.publish_inbound = AsyncMock(return_value=True)
        tool = SpawnTool(provider=provider, bus=bus)

        ctx = _ctx(session_key="weixin:o9cq8004abc@im.wechat")
        result = await tool.execute({"task": "query weather"}, ctx)
        assert result.success is True

        pending = list(tool._tasks.values())
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)

        bus.publish_inbound.assert_called_once()
        event = bus.publish_inbound.call_args[0][0]
        assert event.channel == "weixin"
        assert event.chat_id == "o9cq8004abc@im.wechat"
        assert event.session_key == "weixin:o9cq8004abc@im.wechat"
        assert "weather result" in event.text

    @pytest.mark.asyncio
    async def test_spawn_publishes_gateway_channel(self):
        """Gateway-prefixed session keys must parse correctly."""
        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="done")
        )
        bus = MagicMock()
        bus.publish_inbound = AsyncMock(return_value=True)
        tool = SpawnTool(provider=provider, bus=bus)

        ctx = _ctx(session_key="gateway:weixin:wxid_123")
        await tool.execute({"task": "do thing"}, ctx)

        pending = list(tool._tasks.values())
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)

        event = bus.publish_inbound.call_args[0][0]
        assert event.channel == "gateway:weixin"
        assert event.chat_id == "wxid_123"

    @pytest.mark.asyncio
    async def test_spawn_no_ctx_falls_back_to_system(self):
        """When no context is available, channel falls back to 'system'."""
        provider = MagicMock()
        provider.chat_with_retry = AsyncMock(
            return_value=MagicMock(content="done")
        )
        bus = MagicMock()
        bus.publish_inbound = AsyncMock(return_value=True)
        tool = SpawnTool(provider=provider, bus=bus)

        await tool.execute({"task": "orphan task"}, None)

        pending = list(tool._tasks.values())
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)

        event = bus.publish_inbound.call_args[0][0]
        assert event.channel == "system"

    def test_execution_mode(self):
        tool = SpawnTool(provider=MagicMock(), bus=None)
        assert tool.execution_mode({}) == "side_effect"
