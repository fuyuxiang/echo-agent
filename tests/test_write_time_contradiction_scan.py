"""Tests for write-time contradiction scan — observe-only (flag, never supersede).

See docs/superpowers/specs/2026-06-08-write-time-contradiction-scan-design.md.
"""

import tempfile
from datetime import datetime, timedelta
from pathlib import Path

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(scan_on: bool) -> MemoryStore:
    tmp = tempfile.mkdtemp()
    return MemoryStore(memory_dir=Path(tmp), contradiction_scan_on_store=scan_on)


def _add(store: MemoryStore, key: str, content: str, days_ago: int = 0) -> MemoryEntry:
    ts = (datetime.now() - timedelta(days=days_ago)).isoformat()
    entry = MemoryEntry(type=MemoryType.USER, key=key, content=content, created_at=ts, updated_at=ts)
    return store.add(entry)


class TestObserveOnlyScan:
    def test_flags_but_never_supersedes(self):
        store = _store(scan_on=True)
        # Different full keys, same prefix → not merged by path 1, reaches the scan.
        old = _add(store, "pref:lang", "Python", days_ago=5)
        new = _add(store, "pref:editor", "vim", days_ago=0)

        old_after = store._entries[old.id]
        # Observe-only: older entry flagged, but NOT superseded.
        assert store.SUSPECTED_CONFLICT_TAG in old_after.tags
        assert old_after.superseded_by == ""
        assert new.superseded_by == ""

    def test_disabled_by_default_no_side_effects(self):
        store = _store(scan_on=False)
        old = _add(store, "pref:lang", "Python", days_ago=5)
        _add(store, "pref:editor", "vim", days_ago=0)

        old_after = store._entries[old.id]
        assert store.SUSPECTED_CONFLICT_TAG not in old_after.tags
        assert old_after.superseded_by == ""

    def test_flag_is_idempotent(self):
        store = _store(scan_on=True)
        old = _add(store, "pref:lang", "Python", days_ago=10)
        _add(store, "pref:editor", "vim", days_ago=5)
        _add(store, "pref:shell", "bash", days_ago=0)

        old_after = store._entries[old.id]
        assert old_after.tags.count(store.SUSPECTED_CONFLICT_TAG) == 1

    def test_no_flag_when_different_prefix(self):
        store = _store(scan_on=True)
        a = _add(store, "name", "Alice", days_ago=5)
        _add(store, "location", "Beijing", days_ago=0)

        a_after = store._entries[a.id]
        assert store.SUSPECTED_CONFLICT_TAG not in a_after.tags
