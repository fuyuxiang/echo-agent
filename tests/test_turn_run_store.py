from pathlib import Path

import pytest

from echo_agent.agent.turn_run_store import TurnRunStore
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
