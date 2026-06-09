"""Tests for MemoryStore._save_type and storage sync logic."""

from __future__ import annotations

import asyncio
import json
import uuid
from datetime import datetime
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
import pytest_asyncio

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType


def _make_entry(**overrides: Any) -> MemoryEntry:
    defaults = dict(
        id=uuid.uuid4().hex[:12],
        type=MemoryType.USER,
        tier=MemoryTier.SEMANTIC,
        key="test_key",
        content="test content",
        importance=0.8,
        access_count=0,
        last_accessed="",
        created_at=datetime.now().isoformat(),
        updated_at=datetime.now().isoformat(),
    )
    defaults.update(overrides)
    return MemoryEntry(**defaults)


class TestSaveType:
    """Tests for _save_type file persistence."""

    def test_save_type_writes_json_file(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = _make_entry(key="k1", content="hello world")
        store.add(entry)

        user_file = tmp_path / "mem" / "user_memory.json"
        assert user_file.exists()
        data = json.loads(user_file.read_text())
        assert len(data) == 1
        assert data[0]["key"] == "k1"
        assert data[0]["content"] == "hello world"

    def test_save_type_multiple_entries_sorted(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        e1 = _make_entry(key="b_key", content="second", created_at="2026-01-02T00:00:00")
        e2 = _make_entry(key="a_key", content="first", created_at="2026-01-01T00:00:00")
        store.add(e1)
        store.add(e2)

        user_file = tmp_path / "mem" / "user_memory.json"
        data = json.loads(user_file.read_text())
        assert len(data) == 2
        assert data[0]["created_at"] < data[1]["created_at"]

    def test_save_type_env_entries_separate_file(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = _make_entry(type=MemoryType.ENVIRONMENT, key="env_k", content="env data")
        store.add(entry)

        env_file = tmp_path / "mem" / "env_memory.json"
        assert env_file.exists()
        data = json.loads(env_file.read_text())
        assert len(data) == 1
        assert data[0]["key"] == "env_k"


class TestStorageSync:
    """Tests for _save_type storage backend sync."""

    def test_save_type_calls_storage_backend(self, tmp_path: Path) -> None:
        mock_storage = MagicMock()
        future: asyncio.Future = asyncio.Future()
        future.set_result(None)
        mock_storage.store_memory = MagicMock(return_value=future)

        store = MemoryStore(memory_dir=tmp_path / "mem", storage=mock_storage)
        entry = _make_entry(key="sync_test", content="data")
        store.add(entry)

        assert mock_storage.store_memory.called

    def test_save_type_storage_failure_does_not_crash(self, tmp_path: Path) -> None:
        mock_storage = MagicMock()
        mock_storage.store_memory = MagicMock(side_effect=Exception("DB error"))

        store = MemoryStore(memory_dir=tmp_path / "mem", storage=mock_storage)
        entry = _make_entry(key="fail_test", content="data")
        result = store.add(entry)

        assert result is not None
        user_file = tmp_path / "mem" / "user_memory.json"
        assert user_file.exists()

    def test_pending_storage_tasks_tracked(self, tmp_path: Path) -> None:
        mock_storage = MagicMock()
        future: asyncio.Future = asyncio.Future()
        future.set_result(None)
        mock_storage.store_memory = MagicMock(return_value=future)

        store = MemoryStore(memory_dir=tmp_path / "mem", storage=mock_storage)
        assert hasattr(store, "_pending_storage_tasks")
        assert isinstance(store._pending_storage_tasks, set)


class TestStoreAddUpdateDelete:
    """Tests for MemoryStore CRUD triggering _save_type."""

    def test_add_persists_entry(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = _make_entry(key="add_test", content="added")
        result = store.add(entry)
        assert result.key == "add_test"

        store2 = MemoryStore(memory_dir=tmp_path / "mem")
        found = store2.get(result.id)
        assert found is not None
        assert found.content == "added"

    def test_update_persists_change(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = _make_entry(key="upd_test", content="original")
        added = store.add(entry)
        store.update(added.id, content="modified")

        store2 = MemoryStore(memory_dir=tmp_path / "mem")
        found = store2.get(added.id)
        assert found is not None
        assert found.content == "modified"

    def test_delete_removes_from_disk(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = _make_entry(key="del_test", content="to delete")
        added = store.add(entry)
        store.delete(added.id)

        store2 = MemoryStore(memory_dir=tmp_path / "mem")
        assert store2.get(added.id) is None

    def test_duplicate_entry_not_added(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = _make_entry(key="dup", content="same")
        r1 = store.add(entry)
        entry2 = _make_entry(key="dup", content="same")
        r2 = store.add(entry2)
        assert r1.id == r2.id
        assert len(store.list_all(MemoryType.USER)) == 1


class TestCoreResidentUserMemory:
    """Core philosophy: user facts are resident and durable, never decayed out."""

    def test_stale_user_fact_still_in_snapshot(self, tmp_path: Path) -> None:
        """A birthday mentioned long ago must still appear in the snapshot,
        even when many fresher, higher-importance non-user memories exist."""
        from datetime import timedelta

        store = MemoryStore(memory_dir=tmp_path / "mem")
        ancient = (datetime.now() - timedelta(days=365)).isoformat()
        birthday = _make_entry(
            type=MemoryType.USER,
            key="birthday",
            content="用户公历生日 1988-10-11",
            importance=0.5,
            last_accessed=ancient,
        )
        store.add(birthday)

        # Add many fresh, high-importance environment memories that would
        # out-rank the stale birthday if it were subject to decay.
        for i in range(40):
            store.add(_make_entry(
                type=MemoryType.ENVIRONMENT,
                key=f"env_{i}",
                content=f"fresh environment fact {i}",
                importance=0.9,
                last_accessed=datetime.now().isoformat(),
            ))

        snapshot = store.get_snapshot()
        assert "1988-10-11" in snapshot
        assert "## What I Know About You" in snapshot

    def test_user_overflow_emits_curation_notice(self, tmp_path: Path) -> None:
        """When user facts exceed the budget, a curation notice appears rather
        than silently dropping facts."""
        store = MemoryStore(memory_dir=tmp_path / "mem", user_snapshot_char_limit=200)
        for i in range(30):
            store.add(_make_entry(
                type=MemoryType.USER,
                key=f"fact_{i}",
                content=f"a reasonably long user fact number {i} that consumes budget",
                importance=0.5,
            ))
        snapshot = store.get_snapshot()
        assert "more durable facts about the user not shown" in snapshot


class TestTouchDirtyTracking:
    """Tests that search_keyword marks touched entries as dirty for persistence."""

    def test_search_keyword_marks_dirty(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = store.add(_make_entry(key="color", content="user likes blue"))
        store._dirty_ids.clear()

        results = store.search_keyword("blue")
        assert len(results) == 1
        assert entry.id in store._dirty_ids

    def test_search_scored_marks_dirty(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = store.add(_make_entry(key="lang", content="user prefers python"))
        store._dirty_ids.clear()

        results = store.search_scored("python")
        assert len(results) == 1
        assert entry.id in store._dirty_ids

    def test_touch_increments_access_count(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = store.add(_make_entry(key="food", content="user likes sushi"))
        initial_count = entry.access_count

        store.search_keyword("sushi")
        assert entry.access_count == initial_count + 1


class TestKeyIndex:
    """Tests that the key index accelerates conflict detection."""

    def test_key_index_populated_on_add(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = store.add(_make_entry(key="name", content="Alice"))
        assert entry.id in store._key_index.get("name", set())

    def test_key_index_cleared_on_delete(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        entry = store.add(_make_entry(key="role", content="engineer"))
        assert entry.id in store._key_index.get("role", set())

        store.delete(entry.id)
        role_ids = store._key_index.get("role", set())
        assert entry.id not in role_ids

    def test_find_conflict_uses_index(self, tmp_path: Path) -> None:
        store = MemoryStore(memory_dir=tmp_path / "mem")
        store.add(_make_entry(key="city", content="Beijing"))
        new = _make_entry(key="city", content="Shanghai")
        conflict = store._find_conflict(new)
        assert conflict is not None
        assert conflict.content == "Beijing"
