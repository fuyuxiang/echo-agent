"""Tests for AgentLoop background task handling and inbound event processing."""

from __future__ import annotations

import asyncio
from collections import OrderedDict
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
import pytest_asyncio

from echo_agent.bus.events import InboundEvent, OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus


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
