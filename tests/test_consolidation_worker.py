"""Tests for the ConsolidationWorker."""

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.agent.consolidation import ConsolidationWorker
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.tiers import EpisodicManager, SemanticManager
from echo_agent.session.manager import Session, SessionManager
from echo_agent.storage.sqlite import SQLiteBackend


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
        # _run now re-raises so the DURABLE scheduler tier can retry; collect the
        # exception instead of letting gather propagate it.
        results = await asyncio.gather(*spawned, return_exceptions=True)
        assert any(isinstance(r, RuntimeError) for r in results)

        # Regardless of failure, the session must be released from _pending so a
        # later turn (or a scheduler retry with a fresh factory) can re-drive it.
        assert not worker.is_pending("test:7")

    @pytest.mark.asyncio
    async def test_sleep_consolidate_failure_propagates_for_retry(self, mock_sessions, mock_consolidator):
        """A sleep_consolidate failure must re-raise (so the outer DURABLE handler
        can retry it), instead of being silently swallowed by an inner
        logger.warning. Sleep consolidation is idempotent (boundary committed
        before sleep, empty chunk returns early, sub-steps guard double-writes),
        so a retried re-run is safe."""
        session = Session(key="test:sleepfail")
        session.add_message("user", "msg")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()
        mock_consolidator.sleep_consolidate = AsyncMock(side_effect=RuntimeError("sleep boom"))

        completed_keys = []

        async def on_complete(key):
            completed_keys.append(key)

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=True,
        )

        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule("test:sleepfail", spawn_fn, on_complete=on_complete)
        results = await asyncio.gather(*spawned, return_exceptions=True)

        # The sleep failure must surface as a raised exception, not be swallowed.
        assert any(isinstance(r, RuntimeError) for r in results)
        # on_complete must NOT run when sleep failed — the attempt did not succeed.
        assert completed_keys == []
        # _pending is still released so a scheduler retry can re-drive it.
        assert not worker.is_pending("test:sleepfail")


class TestDurableScheduling:
    """Task 8: consolidation is a DURABLE background point — it must be scheduled
    with tier=DURABLE and as a zero-arg factory (so the scheduler can retry it)."""

    @pytest.mark.asyncio
    async def test_schedule_durable_passes_factory_and_tier(
        self, mock_sessions, mock_consolidator
    ):
        from echo_agent.agent.background import Tier

        session = Session(key="test:dur")
        session.add_message("user", "hello")
        session.add_message("assistant", "hi")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=False,
        )

        captured: dict = {}

        def spawn_fn(coro, *, session_key="", tier=None):
            captured["coro"] = coro
            captured["tier"] = tier

        await worker.schedule("test:dur", spawn_fn, tier=Tier.DURABLE)

        # DURABLE contract: tier flagged and a callable factory (not a bare
        # coroutine), so a failed run can be re-invoked.
        assert captured["tier"] is Tier.DURABLE
        assert callable(captured["coro"])
        assert not asyncio.iscoroutine(captured["coro"])

        # The factory yields a fresh awaitable that actually runs consolidation.
        await captured["coro"]()
        mock_consolidator.consolidate_chunk.assert_called_once()

    @pytest.mark.asyncio
    async def test_durable_retry_actually_recovers_transient_failure(
        self, mock_sessions, mock_consolidator
    ):
        """End-to-end: a transient consolidation failure must be retried by the
        real DURABLE scheduler tier. Regression guard for the bug where _run
        swallowed its own exception, so the scheduler never saw a raise and the
        DURABLE retry path was inert."""
        from echo_agent.agent.background import BackgroundScheduler, Tier

        session = Session(key="test:retry")
        session.add_message("user", "hello")
        session.add_message("assistant", "hi")
        mock_sessions.get_or_create = AsyncMock(return_value=session)
        mock_sessions.save = AsyncMock()

        calls = []

        async def flaky(chunk, memory_scope=""):
            calls.append(1)
            if len(calls) < 2:
                raise RuntimeError("transient")
            return True

        mock_consolidator.consolidate_chunk = AsyncMock(side_effect=flaky)

        worker = ConsolidationWorker(
            sessions=mock_sessions,
            consolidator=mock_consolidator,
            sleep_consolidation=False,
        )

        sched = BackgroundScheduler(max_concurrency=2, durable_retries=2)
        await worker.schedule("test:retry", sched.spawn, tier=Tier.DURABLE)
        await sched.aclose(timeout=5.0)

        # First attempt raised, scheduler retried with a fresh factory coro, and
        # the second attempt committed. _pending is released either way.
        assert len(calls) >= 2
        assert not worker.is_pending("test:retry")


class TestSnapshotValidity:
    """Full-region comparison: a mid-region rewrite (e.g. by compression)
    must invalidate the snapshot even when the tail message is identical."""

    def _session(self, messages, last_consolidated=0):
        from types import SimpleNamespace
        return SimpleNamespace(messages=messages, last_consolidated=last_consolidated)

    def test_valid_when_region_unchanged(self):
        chunk = [
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": "b"},
        ]
        session = self._session(list(chunk) + [{"role": "user", "content": "newer"}])
        assert ConsolidationWorker._snapshot_still_valid(session, 0, chunk, 2) is True

    def test_invalid_when_middle_message_rewritten_but_tail_matches(self):
        chunk = [
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": "b"},
        ]
        # Compression rewrote the first message; the tail is still identical.
        session = self._session([
            {"role": "user", "content": "[compressed summary]"},
            {"role": "assistant", "content": "b"},
        ])
        assert ConsolidationWorker._snapshot_still_valid(session, 0, chunk, 2) is False

    def test_invalid_when_boundary_moved(self):
        chunk = [{"role": "user", "content": "a"}]
        session = self._session([{"role": "user", "content": "a"}], last_consolidated=1)
        assert ConsolidationWorker._snapshot_still_valid(session, 0, chunk, 1) is False

    def test_invalid_when_history_truncated(self):
        chunk = [
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": "b"},
        ]
        session = self._session([{"role": "user", "content": "a"}])
        assert ConsolidationWorker._snapshot_still_valid(session, 0, chunk, 2) is False


class TestEpisodeAbsoluteRange:
    """Critical 回归：巩固轮次的 episode 幂等键必须用绝对消息区间。

    修复前 consolidator 传 message_range=(0, len(messages)) —— range_start 恒 0、
    range_end 是本轮消息条数（相对长度）。两个内容不同但恰好条数相同的巩固轮次会
    生成同一把 (session, 0, N) 幂等键 → 第二轮 episode 被静默去重丢弃。
    修复后应传绝对区间 (start, start+len)，两轮各建一条。
    """

    async def _make_worker(self, tmp_path: Path):
        storage = SQLiteBackend(tmp_path / "db.sqlite")
        await storage.initialize()
        store = MemoryStore(memory_dir=tmp_path / "mem", storage=storage)

        async def mock_llm(**kwargs):
            class _Resp:
                content = "summary"
                tool_calls: list = []
            return _Resp()

        consolidator = MemoryConsolidator(store, mock_llm, consolidation_threshold=1)
        episodic = EpisodicManager(storage)
        consolidator.set_episodic_manager(episodic)
        consolidator.set_semantic_manager(SemanticManager(MemoryService(store)))

        sessions = SessionManager(tmp_path / "sessions", storage=storage)
        worker = ConsolidationWorker(
            sessions=sessions, consolidator=consolidator, sleep_consolidation=True,
        )
        return worker, sessions, storage, episodic

    async def _run_worker(self, worker, session_key: str):
        spawned = []

        def spawn_fn(coro):
            spawned.append(asyncio.ensure_future(coro))

        await worker.schedule(session_key, spawn_fn)
        await asyncio.gather(*spawned)

    @pytest.mark.asyncio
    async def test_two_equal_length_rounds_create_two_episodes(self, tmp_path):
        """两个内容不同、条数相同的巩固轮次（绝对区间不同）各建一条 episode，不误去重。"""
        worker, sessions, storage, _ = await self._make_worker(tmp_path)
        session_key = "chan:1"

        # Round 1: 2 条消息，绝对区间 (0, 2)。
        session = await sessions.get_or_create(session_key)
        session.add_message("user", "第一轮内容 A")
        session.add_message("assistant", "回应 A")
        await sessions.save(session)
        await self._run_worker(worker, session_key)

        # Round 2: 再 2 条消息，绝对区间 (2, 4) —— 条数与第一轮相同但位置不同。
        session = await sessions.get_or_create(session_key)
        session.add_message("user", "第二轮内容 B")
        session.add_message("assistant", "回应 B")
        await sessions.save(session)
        await self._run_worker(worker, session_key)

        rows = await storage.fetch_sql(
            "SELECT COUNT(*) AS n FROM memory_episodes WHERE session_key=?",
            (session_key,),
        )
        assert rows[0]["n"] == 2, "两个等长巩固轮次被相对键误去重，episodic 记忆静默丢失"
