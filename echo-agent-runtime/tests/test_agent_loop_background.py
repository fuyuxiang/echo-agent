"""Tests for AgentLoop background task handling and inbound event processing."""

from __future__ import annotations

import asyncio
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest


@pytest.fixture
def make_loop(tmp_path):
    """Build a real AgentLoop with heavy subsystems disabled."""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.loader import load_config
    from echo_agent.models.provider import LLMProvider, LLMResponse

    class _Stub(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
            return LLMResponse(content="ok")

        def get_default_model(self):
            return "stub"

    def _build():
        config = load_config(overrides={"workspace": str(tmp_path)})
        config.evolution.enabled = False
        config.knowledge.enabled = False
        return AgentLoop(
            bus=MessageBus(),
            config=config,
            provider=_Stub(),
            workspace=tmp_path,
        )

    return _build


def test_loop_has_background_scheduler(make_loop):
    loop = make_loop()
    assert loop._bg_scheduler is not None
    assert loop._bg_scheduler.stats()["dropped"] == 0


@pytest.mark.asyncio
async def test_stop_drains_scheduler_tasks(make_loop):
    """Real AgentLoop: _spawn_background delegates to the scheduler and stop()
    drains it via aclose (the single shutdown path after the dead-code cleanup)."""
    loop = make_loop()

    started = asyncio.Event()

    async def slow_task():
        started.set()
        await asyncio.sleep(100)

    async def quick_task():
        return "done"

    loop._spawn_background(slow_task())
    loop._spawn_background(quick_task())
    await started.wait()
    # Tasks live on the scheduler, not on the loop.
    assert loop._bg_scheduler.stats()["running"] >= 1

    await loop.stop()

    # stop() -> aclose() cancelled/flushed everything; nothing left running.
    assert loop._bg_scheduler.stats()["running"] == 0
    assert len(loop._bg_scheduler._tasks) == 0


@pytest.mark.asyncio
async def test_stop_duck_closes_every_registered_lifecycle_tool(make_loop):
    loop = make_loop()
    closable = MagicMock()
    closable.name = "lifecycle_probe"
    closable.aclose = AsyncMock()
    loop.tools.register(closable)

    await loop.stop()

    closable.aclose.assert_awaited_once_with()


@pytest.mark.asyncio
async def test_stop_preserves_plugin_hook_and_tool_ownership_order(make_loop):
    loop = make_loop()
    order: list[str] = []

    host_tool = MagicMock()
    host_tool.name = "host_lifecycle_probe"
    host_tool.aclose = AsyncMock(side_effect=lambda: order.append("host-close"))
    plugin_tool = MagicMock()
    plugin_tool.name = "plugin_lifecycle_probe"
    plugin_tool.aclose = AsyncMock(side_effect=lambda: order.append("plugin-close"))
    loop.tools.register(host_tool)
    loop.tools.register(plugin_tool)

    async def dispatch(hook_name: str):
        assert hook_name == "on_agent_stop"
        order.append("hook")

    async def shutdown():
        # PluginManager/deactivate owns its registered tool and closes it once.
        await plugin_tool.aclose()
        order.append("plugin-shutdown")

    manager = MagicMock()
    manager.hooks.dispatch = AsyncMock(side_effect=dispatch)
    manager.shutdown = AsyncMock(side_effect=shutdown)
    manager.owns_tool.side_effect = lambda tool: tool is plugin_tool
    loop._plugin_manager = manager

    await loop.stop()

    assert order == ["hook", "host-close", "plugin-close", "plugin-shutdown"]
    host_tool.aclose.assert_awaited_once_with()
    plugin_tool.aclose.assert_awaited_once_with()


@pytest.mark.asyncio
async def test_stop_closes_host_replacement_and_original_plugin_tool_once(make_loop):
    loop = make_loop()
    events: list[str] = []

    original = MagicMock()
    original.name = "replaceable_lifecycle_probe"
    original.aclose = AsyncMock(side_effect=lambda: events.append("original-close"))
    replacement = MagicMock()
    replacement.name = original.name
    replacement.aclose = AsyncMock(
        side_effect=lambda: events.append("replacement-close")
    )
    loop.tools.register(original)
    loop.tools.register(replacement, replace=True)

    async def shutdown():
        await original.aclose()

    manager = MagicMock()
    manager.hooks.dispatch = AsyncMock()
    manager.shutdown = AsyncMock(side_effect=shutdown)
    manager.owns_tool.side_effect = lambda tool: tool is original
    loop._plugin_manager = manager

    await loop.stop()

    assert events == ["replacement-close", "original-close"]
    replacement.aclose.assert_awaited_once_with()
    original.aclose.assert_awaited_once_with()


@pytest.mark.asyncio
async def test_stop_shuts_down_telemetry_best_effort(make_loop):
    loop = make_loop()
    telemetry = MagicMock()
    telemetry.shutdown.side_effect = RuntimeError("exporter already stopped")
    loop._telemetry = telemetry

    # A telemetry exporter failure must not abort the agent's remaining teardown.
    await loop.stop()

    telemetry.shutdown.assert_called_once_with()
    assert loop._bg_scheduler.stats()["running"] == 0



class _FakeAgentLoop:
    """Minimal AgentLoop stand-in for testing _spawn_background and _on_background_done."""

    def __init__(self):
        self._background_tasks: set[asyncio.Task] = set()
        self._errors: list[BaseException] = []

    def _spawn_background(self, coro: Any) -> None:
        task = asyncio.create_task(coro)
        self._background_tasks.add(task)
        task.add_done_callback(self._on_background_done)

    def _on_background_done(self, task: asyncio.Task) -> None:
        self._background_tasks.discard(task)
        if not task.cancelled() and task.exception():
            self._errors.append(task.exception())


class TestSpawnBackground:
    """Tests for background task lifecycle."""

    @pytest.mark.asyncio
    async def test_successful_task_removed_from_set(self) -> None:
        loop = _FakeAgentLoop()

        async def ok_task():
            return "done"

        loop._spawn_background(ok_task())
        assert len(loop._background_tasks) == 1
        await asyncio.gather(*loop._background_tasks)
        await asyncio.sleep(0.01)
        assert len(loop._background_tasks) == 0
        assert len(loop._errors) == 0

    @pytest.mark.asyncio
    async def test_failed_task_logs_exception(self) -> None:
        loop = _FakeAgentLoop()

        async def bad_task():
            raise ValueError("something broke")

        loop._spawn_background(bad_task())
        await asyncio.sleep(0.05)
        assert len(loop._background_tasks) == 0
        assert len(loop._errors) == 1
        assert "something broke" in str(loop._errors[0])

    @pytest.mark.asyncio
    async def test_cancelled_task_no_error(self) -> None:
        loop = _FakeAgentLoop()

        async def slow_task():
            await asyncio.sleep(100)

        loop._spawn_background(slow_task())
        task = next(iter(loop._background_tasks))
        task.cancel()
        await asyncio.sleep(0.05)
        assert len(loop._background_tasks) == 0
        assert len(loop._errors) == 0

    @pytest.mark.asyncio
    async def test_multiple_tasks_tracked(self) -> None:
        loop = _FakeAgentLoop()
        results = []

        async def task_a():
            results.append("a")

        async def task_b():
            results.append("b")

        loop._spawn_background(task_a())
        loop._spawn_background(task_b())
        assert len(loop._background_tasks) == 2
        await asyncio.sleep(0.05)
        assert len(loop._background_tasks) == 0
        assert set(results) == {"a", "b"}
