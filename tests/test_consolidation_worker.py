"""Tests for the ConsolidationWorker."""

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.consolidation import ConsolidationWorker
from echo_agent.session.manager import Session


@pytest.fixture
def mock_sessions():
    mgr = AsyncMock()
    lock = asyncio.Lock()
    mgr.acquire = AsyncMock(return_value=lock)
    return mgr


@pytest.fixture
def mock_consolidator():
    c = AsyncMock()
    c.consolidate_chunk = AsyncMock(return_value=True)
    c.sleep_consolidate = AsyncMock(return_value={"episodes": 1})
    return c


class TestConsolidationWorker:
    @pytest.mark.asyncio
    async def test_run_consolidates_and_updates_boundary(self, mock_sessions, mock_consolidator):
        session = Session(key="test:1")
        session.add_message("user", "hello")
        session.add_message("assistant", "hi")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=False,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:1", spawn_fn)
        await asyncio.gather(*spawned)

        mock_consolidator.consolidate_chunk.assert_called_once()
        mock_sessions.save.assert_called_once()
        assert session.last_consolidated == 2

    @pytest.mark.asyncio
    async def test_skips_empty_chunk(self, mock_sessions, mock_consolidator):
        session = Session(key="test:2")
        session.last_consolidated = 0
        mock_sessions.get_or_create = AsyncMock(return_value=session)

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:2", spawn_fn)
        await asyncio.gather(*spawned)

        mock_consolidator.consolidate_chunk.assert_not_called()

    @pytest.mark.asyncio
    async def test_deduplicates_pending(self, mock_sessions, mock_consolidator):
        session = Session(key="test:3")
        session.add_message("user", "msg")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=False,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:3", spawn_fn)
        await worker.schedule("test:3", spawn_fn)  # should be deduplicated
        assert len(spawned) == 1
        await asyncio.gather(*spawned)

    @pytest.mark.asyncio
    async def test_clears_pending_after_completion(self, mock_sessions, mock_consolidator):
        session = Session(key="test:4")
        session.add_message("user", "msg")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=False,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:4", spawn_fn)
        await asyncio.gather(*spawned)
        assert not worker.is_pending("test:4")

    @pytest.mark.asyncio
    async def test_sleep_consolidation_runs_when_enabled(self, mock_sessions, mock_consolidator):
        session = Session(key="test:5")
        session.add_message("user", "msg")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=True,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:5", spawn_fn)
        await asyncio.gather(*spawned)

        mock_consolidator.sleep_consolidate.assert_called_once()

    @pytest.mark.asyncio
    async def test_on_complete_callback(self, mock_sessions, mock_consolidator):
        session = Session(key="test:6")
        session.add_message("user", "msg")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        completed_keys = []

        async def on_complete(key):
            completed_keys.append(key)

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=False,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:6", spawn_fn, on_complete=on_complete)
        await asyncio.gather(*spawned)

        assert completed_keys == ["test:6"]

    @pytest.mark.asyncio
    async def test_handles_consolidation_error(self, mock_sessions, mock_consolidator):
        session = Session(key="test:7")
        session.add_message("user", "msg")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_consolidator.consolidate_chunk = AsyncMock(side_effect=RuntimeError("boom"))

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:7", spawn_fn)
        await asyncio.gather(*spawned)

        assert not worker.is_pending("test:7")
