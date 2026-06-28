import pytest
import pytest_asyncio

from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import Contradiction
from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def wired(tmp_path):
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    store = MemoryStore(memory_dir=tmp_path / "mem")
    detector = ContradictionDetector(storage=storage, store=store)
    return storage, store, detector


@pytest.mark.asyncio
async def test_store_contradiction_marks_unresolved(wired):
    _storage, store, detector = wired
    c = Contradiction(id="c1", memory_id_a="m1", memory_id_b="m2", description="x")
    await detector.store_contradiction(c)
    assert store.is_unresolved("m1") is True
    assert store.is_unresolved("m2") is True


@pytest.mark.asyncio
async def test_resolve_clears_unresolved(wired):
    _storage, store, detector = wired
    c = Contradiction(id="c1", memory_id_a="m1", memory_id_b="m2", description="x")
    await detector.store_contradiction(c)
    await detector.resolve("c1", "user_decided")
    assert store.is_unresolved("m1") is False
    assert store.is_unresolved("m2") is False
