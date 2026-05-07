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
