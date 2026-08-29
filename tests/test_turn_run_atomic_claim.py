"""A turn event ID has one atomic execution claimant."""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_concurrent_mark_running_has_exactly_one_winner(tmp_path) -> None:
    backend = SQLiteBackend(tmp_path / "claims.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        await store.accept("same-event", "session-a", metadata={"first": True})

        first, second = await asyncio.gather(
            store.mark_running(
                "same-event", "session-a", context_key="ctx-a", trace_id="trace-a",
            ),
            store.mark_running(
                "same-event", "session-a", context_key="ctx-b", trace_id="trace-b",
            ),
        )

        assert sorted([first, second]) == [False, True]
        row = await store.get("same-event")
        assert row is not None and row["status"] == "running"
        winner = "trace-a" if first else "trace-b"
        assert row["trace_id"] == winner
        assert await store.mark_running(
            "same-event", "session-a", context_key="ctx-c", trace_id="trace-c",
        ) is False
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_duplicate_accept_cannot_reassign_session_or_metadata(tmp_path) -> None:
    backend = SQLiteBackend(tmp_path / "claims.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        await store.accept("same-event", "owner-session", metadata={"owner": "a"})
        await store.accept("same-event", "foreign-session", metadata={"owner": "b"})
        row = await store.get("same-event")
        assert row is not None
        assert row["session_key"] == "owner-session"
        assert row["metadata"] == {"owner": "a"}
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_release_acceptance_only_deletes_matching_unstarted_row(tmp_path) -> None:
    backend = SQLiteBackend(tmp_path / "release.db")
    await backend.initialize()
    try:
        store = TurnRunStore(backend)
        await store.accept("retryable", "session-a")
        assert await store.release_acceptance("retryable", "session-b") is False
        assert await store.get("retryable") is not None
        assert await store.release_acceptance("retryable", "session-a") is True
        assert await store.get("retryable") is None

        await store.accept("running", "session-a")
        assert await store.mark_running(
            "running", "session-a", context_key="ctx", trace_id="trace",
        )
        assert await store.release_acceptance("running", "session-a") is False
        row = await store.get("running")
        assert row is not None and row["status"] == "running"
    finally:
        await backend.close()
