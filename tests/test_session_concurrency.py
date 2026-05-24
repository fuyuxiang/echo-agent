"""Tests for SessionManager concurrency control and cache eviction."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from echo_agent.session.manager import Session, SessionManager


@pytest.fixture
def manager(tmp_path: Path) -> SessionManager:
    mgr = SessionManager(sessions_dir=tmp_path / "sessions")
    mgr._max_cache_size = 3
    mgr._max_session_locks = 3
    return mgr


@pytest.mark.asyncio
async def test_acquire_returns_same_lock_for_same_key(manager: SessionManager) -> None:
    lock1 = await manager.acquire("session:a")
    lock2 = await manager.acquire("session:a")
    assert lock1 is lock2


@pytest.mark.asyncio
async def test_acquire_returns_different_lock_for_different_key(manager: SessionManager) -> None:
    lock1 = await manager.acquire("session:a")
    lock2 = await manager.acquire("session:b")
    assert lock1 is not lock2


@pytest.mark.asyncio
async def test_acquire_evicts_oldest_lock_when_full(manager: SessionManager) -> None:
    lock_a = await manager.acquire("session:a")
    await manager.acquire("session:b")
    await manager.acquire("session:c")
    await manager.acquire("session:d")  # should evict "session:a"

    lock_a_new = await manager.acquire("session:a")
    assert lock_a_new is not lock_a  # old lock was evicted, new one created


@pytest.mark.asyncio
async def test_cache_eviction_saves_session(tmp_path: Path) -> None:
    manager = SessionManager(sessions_dir=tmp_path / "sessions")
    manager._max_cache_size = 2

    s1 = await manager.get_or_create("ch:chat1")
    s1.add_message("user", "hello from chat1")
    await manager.save(s1)

    await manager.get_or_create("ch:chat2")
    await manager.get_or_create("ch:chat3")  # evicts chat1

    # chat1 should have been saved before eviction
    path = manager._session_path("ch:chat1")
    assert path.exists()

    # reload and verify content preserved
    manager2 = SessionManager(sessions_dir=tmp_path / "sessions")
    loaded = await manager2.get_or_create("ch:chat1")
    assert any(m.get("content") == "hello from chat1" for m in loaded.messages)


@pytest.mark.asyncio
async def test_concurrent_messages_serialized(manager: SessionManager) -> None:
    order: list[int] = []

    async def process(session_key: str, idx: int) -> None:
        lock = await manager.acquire(session_key)
        async with lock:
            order.append(idx)
            await asyncio.sleep(0.01)
            order.append(idx)

    await asyncio.gather(
        process("session:x", 1),
        process("session:x", 2),
    )

    # With serialization, we expect [1,1,2,2] or [2,2,1,1], not interleaved
    assert order == [1, 1, 2, 2] or order == [2, 2, 1, 1]


@pytest.mark.asyncio
async def test_held_lock_not_evicted_when_cache_full(manager: SessionManager) -> None:
    """A lock that's currently held must NOT be replaced by LRU eviction —
    otherwise a second caller would create a fresh Lock for the same key
    and break mutual exclusion. The held entry is skipped; an unheld entry
    is evicted instead.
    """
    held_lock = await manager.acquire("session:hold")
    async with held_lock:
        # Fill the cache past _max_session_locks (=3) so eviction must run.
        await manager.acquire("session:b")
        await manager.acquire("session:c")
        await manager.acquire("session:d")  # cache now over limit

        # Re-acquiring the held key MUST return the same lock instance —
        # if it were evicted, this would be a new Lock and the outer
        # `async with held_lock` would no longer protect anything.
        same_lock = await manager.acquire("session:hold")
        assert same_lock is held_lock


@pytest.mark.asyncio
async def test_eviction_falls_back_to_growth_when_all_locked(manager: SessionManager) -> None:
    """If every lock in the cache is currently held, the cache is allowed
    to grow rather than violate mutual exclusion."""

    async def hold(key: str, gate: asyncio.Event) -> None:
        lock = await manager.acquire(key)
        async with lock:
            await gate.wait()

    gate = asyncio.Event()
    holders = [
        asyncio.create_task(hold(f"session:hold{i}", gate))
        for i in range(3)
    ]
    # Yield until every holder has actually entered its critical section.
    while True:
        if all(
            manager._session_locks.get(f"session:hold{i}") is not None
            and manager._session_locks[f"session:hold{i}"].locked()
            for i in range(3)
        ):
            break
        await asyncio.sleep(0)

    # All 3 slots are held. Asking for a fresh key must succeed without
    # nuking any held entry.
    new_lock = await manager.acquire("session:new")
    assert new_lock is not None
    for i in range(3):
        assert f"session:hold{i}" in manager._session_locks

    gate.set()
    await asyncio.gather(*holders)
