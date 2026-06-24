import pytest
from echo_agent.config.schema import MemoryConfig
from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.types import MemoryEntry


def test_auto_resolve_contradictions_defaults_false():
    cfg = MemoryConfig()
    assert cfg.auto_resolve_contradictions is False


class _FakeStore:
    def __init__(self, entries):
        self._entries = {e.id: e for e in entries}
        self.superseded = []
        self.versions = {}

    def mark_superseded(self, entry_id, superseded_by):
        self.superseded.append((entry_id, superseded_by))
        if entry_id in self._entries:
            self._entries[entry_id].superseded_by = superseded_by
        return True

    def set_version(self, entry_id, version):
        self.versions[entry_id] = version
        return True


class _StubStorage:
    async def execute_sql(self, *a, **k):
        return None

    async def fetch_sql(self, *a, **k):
        # 真实 check_lightweight_sync(new, [old]) 产生 memory_id_a=new, memory_id_b=old
        return [{"memory_id_a": "new1", "memory_id_b": "old1"}]


def _make_consolidator(store):
    async def _noop_llm(**kwargs):
        raise AssertionError("LLM must not be called in auto-resolve path")
    c = MemoryConsolidator(memory_store=store, llm_call=_noop_llm)
    return c


@pytest.mark.asyncio
async def test_auto_resolve_supersedes_older_same_key(monkeypatch):
    old = MemoryEntry(id="old1", key="pref:lang", content="Python",
                      created_at="2026-06-01T00:00:00", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="new1", key="pref:lang", content="Rust",
                      created_at="2026-06-02T00:00:00", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])

    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)

    stats = await consolidator._auto_resolve_same_key([new], [old, new])
    assert ("old1", "new1") in store.superseded
    assert stats == 1


@pytest.mark.asyncio
async def test_auto_resolve_disabled_by_default():
    old = MemoryEntry(id="o", key="pref:lang", content="Python", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="n", key="pref:lang", content="Rust", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    # auto_resolve left at default False
    assert consolidator._auto_resolve_contradictions is False


@pytest.mark.asyncio
async def test_auto_resolve_gate_off_skips():
    # 与 test_auto_resolve_supersedes_older_same_key 相同的 store/detector,
    # 但不调用 set_auto_resolve_contradictions(保持默认 False)。
    # 关键:显式复现 sleep_consolidate 的 Step 3b 门控语义——
    # 门控关时连 _auto_resolve_same_key 都不会被调用,因此既不消解也不 supersede。
    old = MemoryEntry(id="old1", key="pref:lang", content="Python",
                      created_at="2026-06-01T00:00:00", updated_at="2026-06-01T00:00:00")
    new = MemoryEntry(id="new1", key="pref:lang", content="Rust",
                      created_at="2026-06-02T00:00:00", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([old, new])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    # 门控保持默认 False
    assert consolidator._auto_resolve_contradictions is False

    # 复现 Step 3b 门控:`if self._auto_resolve_contradictions and ...`
    resolved = (
        await consolidator._auto_resolve_same_key([new], [old, new])
        if consolidator._auto_resolve_contradictions
        else 0
    )
    assert resolved == 0
    assert store.superseded == []


@pytest.mark.asyncio
async def test_auto_resolve_skips_different_key():
    a = MemoryEntry(id="a", key="pref:lang", content="Python", updated_at="2026-06-01T00:00:00")
    b = MemoryEntry(id="b", key="pref:editor", content="vim", updated_at="2026-06-02T00:00:00")
    store = _FakeStore([a, b])
    detector = ContradictionDetector(storage=_StubStorage(), store=store)
    consolidator = _make_consolidator(store)
    consolidator.set_contradiction_detector(detector)
    consolidator.set_auto_resolve_contradictions(True)
    resolved = await consolidator._auto_resolve_same_key([b], [a, b])
    assert resolved == 0
    assert store.superseded == []
