"""Lifecycle guarantees for Dashboard background jobs."""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.gateway.jobs import AsyncJobRegistry


@pytest.mark.asyncio
async def test_immediate_cancel_always_reaches_terminal_state() -> None:
    registry = AsyncJobRegistry()

    async def never_finishes() -> None:
        await asyncio.Event().wait()

    job = registry.start("rebuild", never_finishes)
    assert await registry.cancel(job["id"]) is True
    state = registry.get(job["id"])
    assert state is not None
    assert state["status"] == "cancelled"
    assert state["progress"] == 100
    assert state["completed_at"] is not None


@pytest.mark.asyncio
async def test_cancel_does_not_claim_success_after_irreversible_completion() -> None:
    registry = AsyncJobRegistry()
    entered = asyncio.Event()
    release = asyncio.Event()

    async def commits_despite_late_cancel() -> str:
        entered.set()
        try:
            await release.wait()
        except asyncio.CancelledError:
            # Models an operation which has crossed an irreversible commit
            # boundary and truthfully completes instead of pretending to stop.
            return "committed"
        return "committed"

    job = registry.start("commit", commits_despite_late_cancel)
    await entered.wait()
    assert await registry.cancel(job["id"]) is False
    state = registry.get(job["id"])
    assert state is not None
    assert state["status"] == "completed"
    assert state["result"] == "committed"


@pytest.mark.asyncio
async def test_failed_job_is_observable_and_does_not_escape() -> None:
    async def fails() -> None:
        raise RuntimeError("index exploded")

    registry = AsyncJobRegistry()
    job = registry.start("rebuild", fails)
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    state = registry.get(job["id"])
    assert state is not None
    assert state["status"] == "failed"
    assert state["error"] == "index exploded"


@pytest.mark.asyncio
async def test_registry_returns_to_bound_after_active_burst_finishes() -> None:
    release = asyncio.Event()

    async def blocked() -> str:
        await release.wait()
        return "ok"

    registry = AsyncJobRegistry(max_jobs=10)
    for _ in range(14):
        registry.start("upload", blocked)
    assert len(registry._jobs) == 14

    release.set()
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert len(registry._jobs) <= 10


@pytest.mark.asyncio
async def test_lifecycle_events_include_completion() -> None:
    events: list[dict] = []

    async def sink(_event_type: str, payload: dict) -> None:
        events.append(payload)

    registry = AsyncJobRegistry(event_sink=sink)
    job = registry.start("upload", lambda: asyncio.sleep(0, result={"indexed": 2}))
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert any(event["id"] == job["id"] and event["status"] == "completed" for event in events)
