"""Concurrency test: update() interleaved during flush's embedding await must not
misassign the stale vector to new content, nor drop the freshly-queued re-embed."""
from pathlib import Path

import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


class _FakeIndex:
    def __init__(self):
        self.n = 0
        self.removed = []

    async def add(self, entry_id, embedding):
        self.n += 1
        return f"vec_{self.n}"

    async def remove(self, vec_id):
        self.removed.append(vec_id)


@pytest.fixture
def memory_store_factory(tmp_path: Path):
    # Minimal real construction (mirrors tests/test_memory_store_save.py): a
    # MemoryStore rooted at tmp_path, no SQL storage needed for this test since
    # the vector index is faked and JSON persistence uses the temp dir.
    def _factory() -> MemoryStore:
        return MemoryStore(memory_dir=tmp_path / "memory")

    return _factory


@pytest.mark.asyncio
async def test_update_during_flush_does_not_misassign_or_drop(memory_store_factory):
    """While flush awaits embedding for the OLD text, update() rewrites the entry
    and re-queues. The old vector must NOT attach to the new content, and the new
    pending item must survive for the next flush."""
    import asyncio

    store = memory_store_factory()
    store.set_vector_index(_FakeIndex())

    started = asyncio.Event()

    async def embed_fn(text):
        if text.endswith("old content") or "old content" in text:
            started.set()
            await asyncio.sleep(0.05)  # yield so update() can interleave
        return [0.1, 0.2, 0.3]

    # Wire embed_fn AND vector index before add(): _queue_embed only enqueues
    # when both are set, so the entry lands in _pending_embeds for the flush.
    store.set_embed_fn(embed_fn)

    # key="" so the embedded text is exactly the content.
    entry = store.add(MemoryEntry(type=MemoryType.USER, key="", content="old content"))
    eid = entry.id

    async def racing_update():
        await started.wait()
        store.update(eid, content="new content")

    flush_task = asyncio.create_task(store.flush_pending_embeds())
    upd_task = asyncio.create_task(racing_update())
    await asyncio.gather(flush_task, upd_task)

    # the stale (old-text) vector must not be committed to the now-new entry
    got = store._entries[eid]
    assert got.content == "new content"
    # the stale vector must not have been attached to the new content
    assert got.embedding_id != "vec_1"
    # new content still queued for re-embed (not silently dropped)
    assert any(item[0] == eid for item in store._pending_embeds)

    # second flush embeds the new content cleanly
    await store.flush_pending_embeds()
    assert not any(item[0] == eid for item in store._pending_embeds)
    assert store._entries[eid].embedding_id  # a vector was assigned to new content
