import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.bus.rate_limiter import SessionRateLimiter
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_turn_lifecycle_is_durable_and_terminal_is_monotonic(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "turns.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        await store.accept(
            "evt-1", "cli:local", context_key="cli:local::epoch:2",
            metadata={"channel": "gateway:cli"},
        )
        await store.mark_running(
            "evt-1", "cli:local", context_key="cli:local::epoch:2", trace_id="t1",
        )
        await store.mark_activity(
            "evt-1", status="waiting_clarification", current_tool="clarify",
        )
        waiting = await store.get("evt-1")
        assert waiting is not None
        assert waiting["status"] == "waiting_clarification"
        assert waiting["current_tool"] == "clarify"
        assert waiting["metadata"] == {"channel": "gateway:cli"}

        await store.mark_terminal(
            "evt-1", "incomplete", response_text="partial", error="output_truncated",
        )
        # A late activity frame or duplicate terminal callback cannot resurrect
        # or rewrite a terminal turn.
        await store.mark_activity("evt-1", status="running", current_tool="exec")
        await store.mark_terminal("evt-1", "completed", response_text="wrong")
        done = await store.latest("cli:local")
        assert done is not None
        assert done["status"] == "incomplete"
        assert done["response_text"] == "partial"
        assert done["error"] == "output_truncated"
        assert done["current_tool"] == ""
        assert done["completed_at"]
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_idempotency_tombstone_terminal_status_is_monotonic(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "terminal-tombstone.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        assert (await store.claim_idempotency(
            "evt-keyed",
            namespace="gateway-message",
            fingerprint="body",
            session_key="session",
        ))["outcome"] == "new"
        await store.accept("evt-keyed", "session")
        await store.mark_terminal(
            "evt-keyed", "incomplete", response_text="partial", error="budget",
        )
        await store.mark_terminal(
            "evt-keyed", "completed", response_text="late overwrite",
        )

        tombstone = await store.get_idempotency("evt-keyed")
        assert tombstone is not None
        assert tombstone["status"] == "incomplete"
        assert tombstone["response_text"] == "partial"
        assert tombstone["error"] == "budget"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_turn_listing_is_session_scoped(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "turns.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        await store.accept("a", "s1")
        await store.accept("b", "s2")
        assert [row["event_id"] for row in await store.list_session("s1")] == ["a"]
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_retention_never_prunes_live_rows_and_converges_after_terminal(
    tmp_path: Path,
):
    backend = SQLiteBackend(tmp_path / "turn-retention.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        store._MAX_RUNS_PER_SESSION = 2
        await store.accept("long-running", "same-session")
        assert await store.mark_running(
            "long-running",
            "same-session",
            context_key="ctx",
            trace_id="trace",
        )

        # A burst may temporarily exceed terminal-result retention. None of
        # these authoritative nonterminal rows may be removed.
        for event_id in ("queued-1", "queued-2", "queued-3"):
            await store.accept(event_id, "same-session")
        live_rows = await backend.fetch_sql(
            "SELECT event_id, status FROM turn_runs WHERE session_key=?",
            ("same-session",),
        )
        assert {row["event_id"] for row in live_rows} == {
            "long-running", "queued-1", "queued-2", "queued-3",
        }

        for event_id in ("queued-1", "queued-2", "queued-3"):
            await store.mark_terminal(event_id, "completed")
        assert (await store.get("long-running"))["status"] == "running"
        rows = await backend.fetch_sql(
            "SELECT event_id, status FROM turn_runs WHERE session_key=?",
            ("same-session",),
        )
        assert len(rows) == 3
        assert sum(row["status"] == "completed" for row in rows) == 2

        # Once the long turn becomes terminal, its transition also runs prune;
        # the session settles back to the configured result bound.
        await store.mark_terminal("long-running", "completed")
        rows = await backend.fetch_sql(
            "SELECT status FROM turn_runs WHERE session_key=?",
            ("same-session",),
        )
        assert len(rows) == 2
        assert all(row["status"] == "completed" for row in rows)
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_idempotency_tombstone_outlives_turn_result_retention(
    tmp_path: Path,
):
    db_path = tmp_path / "idempotency-retention.db"
    backend = SQLiteBackend(db_path)
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        claimed = await store.claim_idempotency(
            "keyed-old",
            namespace="gateway-message",
            fingerprint="fingerprint-old",
            session_key="same-session",
        )
        assert claimed["outcome"] == "new"
        await store.accept("keyed-old", "same-session")
        assert await store.mark_running(
            "keyed-old", "same-session", context_key="ctx", trace_id="trace",
        )
        await store.mark_terminal(
            "keyed-old", "completed", response_text="durable answer",
        )

        # Exceed the real production retention boundary. The rich result row
        # is pruned at 500, while the independent keyed-delivery tombstone must
        # remain authoritative for the full idempotency window.
        for index in range(501):
            event_id = f"newer-{index}"
            await store.accept(event_id, "same-session")
            await store.mark_terminal(event_id, "completed")

        assert await store.get("keyed-old") is None
        rows = await backend.fetch_sql(
            "SELECT COUNT(*) AS count FROM turn_runs WHERE session_key=?",
            ("same-session",),
        )
        assert rows[0]["count"] == store._MAX_RUNS_PER_SESSION
    finally:
        await backend.close()

    # Recreate both the SQLite connection and TurnRunStore to exercise a real
    # process-local cache loss/restart, not merely another call on one object.
    reopened = SQLiteBackend(db_path)
    await reopened.initialize()
    try:
        restarted_store = TurnRunStore(reopened)
        replay = await restarted_store.claim_idempotency(
            "keyed-old",
            namespace="gateway-message",
            fingerprint="fingerprint-old",
            session_key="same-session",
        )
        assert replay["outcome"] == "duplicate"
        assert replay["row"]["status"] == "completed"
        assert replay["row"]["response_text"] == "durable answer"

        conflict = await restarted_store.claim_idempotency(
            "keyed-old",
            namespace="gateway-message",
            fingerprint="changed",
            session_key="same-session",
        )
        assert conflict["outcome"] == "conflict"
    finally:
        await reopened.close()


@pytest.mark.asyncio
async def test_idempotency_capacity_fails_closed_until_expiry(tmp_path: Path):
    now = [1_000.0]
    backend = SQLiteBackend(tmp_path / "idempotency-capacity.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend, clock=lambda: now[0])
        store._MAX_IDEMPOTENCY_RECORDS = 2

        for event_id in ("first", "second"):
            result = await store.claim_idempotency(
                event_id,
                namespace="gateway-message",
                fingerprint=f"fingerprint-{event_id}",
                session_key="session",
            )
            assert result["outcome"] == "new"

        full = await store.claim_idempotency(
            "third",
            namespace="gateway-message",
            fingerprint="fingerprint-third",
            session_key="session",
        )
        assert full == {"outcome": "full"}
        rows = await backend.fetch_sql(
            "SELECT event_id FROM inbound_idempotency ORDER BY event_id",
        )
        assert [row["event_id"] for row in rows] == ["first", "second"]

        # Active tombstones use claim time as a bounded backstop. Once both
        # expire, capacity is released and the rejected key can be admitted.
        now[0] += store._IDEMPOTENCY_TTL_SECONDS + 1
        admitted = await store.claim_idempotency(
            "third",
            namespace="gateway-message",
            fingerprint="fingerprint-third",
            session_key="session",
        )
        assert admitted["outcome"] == "new"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_terminal_idempotency_ttl_starts_when_turn_completes(tmp_path: Path):
    now = [10_000.0]
    backend = SQLiteBackend(tmp_path / "idempotency-completion-ttl.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend, clock=lambda: now[0])
        claim = await store.claim_idempotency(
            "slow-turn",
            namespace="gateway-message",
            fingerprint="slow-fingerprint",
            session_key="session",
        )
        assert claim["outcome"] == "new"
        await store.accept("slow-turn", "session")

        # Complete just before the in-flight backstop. Retention must restart
        # here, otherwise a slow request would have essentially no replay TTL.
        now[0] += store._IDEMPOTENCY_TTL_SECONDS - 1
        await store.mark_terminal(
            "slow-turn", "completed", response_text="slow answer",
        )

        now[0] += 2
        replay = await store.claim_idempotency(
            "slow-turn",
            namespace="gateway-message",
            fingerprint="slow-fingerprint",
            session_key="session",
        )
        assert replay["outcome"] == "duplicate"
        assert replay["row"]["response_text"] == "slow answer"

        now[0] += store._IDEMPOTENCY_TTL_SECONDS
        expired = await store.claim_idempotency(
            "slow-turn",
            namespace="gateway-message",
            fingerprint="slow-fingerprint",
            session_key="session",
        )
        assert expired["outcome"] == "new"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_startup_reconciliation_fails_all_orphaned_nonterminal_rows(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "turns.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        await store.accept("accepted", "s1")
        await store.accept("running", "s1")
        await store.mark_running(
            "running", "s1", context_key="s1", trace_id="trace",
        )
        await store.accept("waiting", "s1")
        await store.mark_activity(
            "waiting", status="waiting_approval", current_tool="exec",
        )
        await store.accept("done", "s1")
        await store.mark_terminal("done", "completed", response_text="ok")

        reconciled = await store.reconcile_orphaned()

        assert reconciled == 3
        for event_id in ("accepted", "running", "waiting"):
            row = await store.get(event_id)
            assert row is not None
            assert row["status"] == "failed"
            assert row["error"] == "process restarted"
            assert row["completed_at"]
            assert row["current_tool"] == ""
        done = await store.get("done")
        assert done is not None
        assert done["status"] == "completed"
        assert done["response_text"] == "ok"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_agent_start_runs_reconciliation_before_other_initialization():
    loop = AgentLoop.__new__(AgentLoop)
    loop.bus = MagicMock()
    loop._turn_runs = MagicMock()
    loop._turn_runs.reconcile_orphaned = AsyncMock(return_value=2)
    loop._storage = MagicMock()
    loop._resolve_embed_and_index = AsyncMock(side_effect=RuntimeError("stop after reconcile"))

    with pytest.raises(RuntimeError, match="stop after reconcile"):
        await loop.start()

    loop._turn_runs.reconcile_orphaned.assert_awaited_once_with()
    loop.bus.subscribe_inbound_rejected.assert_called_once_with(loop._on_inbound_rejected)


def _ledger_rejection_sink(store: TurnRunStore) -> AgentLoop:
    loop = AgentLoop.__new__(AgentLoop)
    loop._turn_runs = store
    loop._scheduler = None
    loop._task_manager = None
    return loop


@pytest.mark.asyncio
async def test_sixth_bus_rate_limited_event_is_terminal_not_accepted(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "rate-limit-turns.db")
    await backend.initialize()
    bus = MessageBus(max_queue_size=20)
    bus.set_rate_limiter(SessionRateLimiter(rpm=60, burst=5))
    store = TurnRunStore(backend)
    loop = _ledger_rejection_sink(store)
    replies: list[OutboundEvent] = []

    async def complete(event: InboundEvent) -> None:
        claimed = await store.mark_running(
            event.event_id, event.session_key,
            context_key=event.session_key, trace_id=event.event_id,
        )
        if claimed:
            await store.mark_terminal(event.event_id, "completed")

    bus.subscribe_inbound(complete)
    bus.subscribe_inbound_rejected(loop._on_inbound_rejected)

    async def capture_reply(event: OutboundEvent) -> None:
        replies.append(event)

    bus.subscribe_outbound_global(capture_reply)
    events = [
        InboundEvent.text_message("test", "u", "same", f"message-{index}")
        for index in range(6)
    ]
    try:
        for event in events:
            await store.accept(event.event_id, event.session_key)
            assert await bus.publish_inbound(event)
        await bus.start()
        await bus.stop()

        rows = [await store.get(event.event_id) for event in events]
        assert [row["status"] for row in rows if row is not None] == [
            "completed", "completed", "completed", "completed", "completed", "failed",
        ]
        assert rows[-1] is not None
        assert rows[-1]["error"] == "rate limited"
        assert len(replies) == 1
        assert replies[0].is_final is True
        assert replies[0].message_kind == "final"
        assert replies[0].metadata["_inbound_event_id"] == events[-1].event_id
        assert replies[0].metadata["_error"] is True
        assert replies[0].metadata["_error_reason"] == "rate limited"
        assert replies[0].metadata["_http_status"] == 429
    finally:
        await bus.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_bus_stop_drains_events_that_are_still_queued(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "shutdown-turns.db")
    await backend.initialize()
    bus = MessageBus(max_queue_size=20, max_concurrency=1)
    store = TurnRunStore(backend)
    loop = _ledger_rejection_sink(store)

    async def complete(event: InboundEvent) -> None:
        claimed = await store.mark_running(
            event.event_id, event.session_key,
            context_key=event.session_key, trace_id=event.event_id,
        )
        if claimed:
            await store.mark_terminal(event.event_id, "completed")

    bus.subscribe_inbound(complete)
    bus.subscribe_inbound_rejected(loop._on_inbound_rejected)
    events = [
        InboundEvent.text_message("test", "u", f"chat-{index}", "queued")
        for index in range(3)
    ]
    try:
        # Preload exactly as a burst accepted just before shutdown. start() only
        # schedules the dispatcher; stop() runs in the same tick, so these rows
        # exercise its explicit queued-event drain.
        for event in events:
            await store.accept(event.event_id, event.session_key)
            assert await bus.publish_inbound(event)
        await bus.start()
        await bus.stop()

        rows = [await store.get(event.event_id) for event in events]
        assert [row["status"] for row in rows if row is not None] == [
            "completed", "completed", "completed",
        ]
    finally:
        await bus.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_publish_after_bus_stop_releases_acceptance_for_same_key_retry(
    tmp_path: Path,
):
    backend = SQLiteBackend(tmp_path / "stopped-bus-turns.db")
    await backend.initialize()
    bus = MessageBus()
    store = TurnRunStore(backend)
    loop = _ledger_rejection_sink(store)
    bus.subscribe_inbound_rejected(loop._on_inbound_rejected)
    event = InboundEvent.text_message("test", "u", "late", "too late")

    async def complete(inbound: InboundEvent) -> None:
        claimed = await store.mark_running(
            inbound.event_id,
            inbound.session_key,
            context_key=inbound.session_key,
            trace_id=inbound.event_id,
        )
        if claimed:
            await store.mark_terminal(inbound.event_id, "completed")

    bus.subscribe_inbound(complete)
    try:
        await store.accept(event.event_id, event.session_key)
        await bus.stop()

        assert await bus.publish_inbound(event) is False
        row = await store.get(event.event_id)
        assert row is not None
        assert row["status"] == "accepted"

        # A definitive False means the bus never accepted ownership. Ingress
        # can therefore release the provisional ledger claim, and the same
        # deterministic event/key remains retryable after the bus restarts.
        assert await store.release_acceptance(event.event_id, event.session_key)
        assert await store.get(event.event_id) is None

        await bus.start()
        await store.accept(event.event_id, event.session_key)
        assert await bus.publish_inbound(event) is True
        await bus.stop()
        assert (await store.get(event.event_id))["status"] == "completed"
    finally:
        await bus.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_queue_full_releases_acceptance_for_same_key_retry(tmp_path: Path):
    class RejectOnceQueue(asyncio.Queue):
        def __init__(self) -> None:
            super().__init__()
            self.reject_next = True

        async def put(self, item) -> None:
            if self.reject_next:
                self.reject_next = False
                raise asyncio.TimeoutError
            await super().put(item)

    backend = SQLiteBackend(tmp_path / "full-bus-turns.db")
    await backend.initialize()
    bus = MessageBus()
    bus._inbound_queue = RejectOnceQueue()
    store = TurnRunStore(backend)
    loop = _ledger_rejection_sink(store)
    bus.subscribe_inbound_rejected(loop._on_inbound_rejected)
    event = InboundEvent.text_message("test", "u", "full", "retry me")

    async def complete(inbound: InboundEvent) -> None:
        claimed = await store.mark_running(
            inbound.event_id,
            inbound.session_key,
            context_key=inbound.session_key,
            trace_id=inbound.event_id,
        )
        if claimed:
            await store.mark_terminal(inbound.event_id, "completed")

    bus.subscribe_inbound(complete)
    try:
        await store.accept(event.event_id, event.session_key)
        assert await bus.publish_inbound(event) is False
        assert (await store.get(event.event_id))["status"] == "accepted"

        assert await store.release_acceptance(event.event_id, event.session_key)
        assert await store.get(event.event_id) is None

        await bus.start()
        await store.accept(event.event_id, event.session_key)
        assert await bus.publish_inbound(event) is True
        await bus.stop()
        assert (await store.get(event.event_id))["status"] == "completed"
    finally:
        await bus.stop()
        await backend.close()
