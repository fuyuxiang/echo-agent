"""Tests for enhanced contradiction detection — temporal conflicts and high-similarity cross-key."""

from datetime import datetime, timedelta

from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.types import MemoryEntry


def _make_entry(id: str, key: str, content: str, days_ago: int = 0) -> MemoryEntry:
    ts = (datetime.now() - timedelta(days=days_ago)).isoformat()
    return MemoryEntry(
        id=id,
        key=key,
        content=content,
        created_at=ts,
        updated_at=ts,
    )


class TestTemporalConflictCheck:
    def setup_method(self):
        self.detector = ContradictionDetector(storage=None)

    def test_detects_temporal_conflict_same_key_prefix(self):
        old = _make_entry("a1", "preference:color", "user likes blue", days_ago=5)
        new = _make_entry("a2", "preference:color", "user likes red", days_ago=0)
        result = self.detector._temporal_conflict_check(new, old)
        assert result is not None
        assert result.resolution is None
        assert result.memory_id_a == old.id
        assert result.memory_id_b == new.id

    def test_no_conflict_same_content(self):
        old = _make_entry("a1", "preference:color", "user likes blue", days_ago=5)
        new = _make_entry("a2", "preference:color", "user likes blue", days_ago=0)
        result = self.detector._temporal_conflict_check(new, old)
        assert result is None

    def test_no_conflict_different_key_prefix(self):
        old = _make_entry("a1", "preference:color", "user likes blue", days_ago=5)
        new = _make_entry("a2", "location:city", "user lives in Beijing", days_ago=0)
        result = self.detector._temporal_conflict_check(new, old)
        assert result is None

    def test_no_conflict_within_threshold(self):
        old = _make_entry("a1", "preference:color", "user likes blue", days_ago=0)
        new = _make_entry("a2", "preference:color", "user likes red", days_ago=0)
        result = self.detector._temporal_conflict_check(new, old)
        assert result is None

    def test_no_conflict_missing_key(self):
        old = _make_entry("a1", "", "user likes blue", days_ago=5)
        new = _make_entry("a2", "", "user likes red", days_ago=0)
        result = self.detector._temporal_conflict_check(new, old)
        assert result is None


class TestCheckLightweightSync:
    def setup_method(self):
        self.detector = ContradictionDetector(storage=None)

    def test_detects_heuristic_contradiction(self):
        old = _make_entry("a1", "name", "Alice", days_ago=5)
        new = _make_entry("a2", "name", "Bob", days_ago=0)
        results = self.detector.check_lightweight_sync(new, [old])
        assert len(results) == 1
        assert "name" in results[0].description

    def test_detects_temporal_contradiction(self):
        old = _make_entry("a1", "pref:lang", "Python", days_ago=5)
        new = _make_entry("a2", "pref:lang", "Rust", days_ago=0)
        results = self.detector.check_lightweight_sync(new, [old])
        assert len(results) == 1

    def test_no_contradiction_different_keys(self):
        old = _make_entry("a1", "name", "Alice", days_ago=5)
        new = _make_entry("a2", "location", "Beijing", days_ago=0)
        results = self.detector.check_lightweight_sync(new, [old])
        assert len(results) == 0


class TestStrongSimilarityThreshold:
    def test_threshold_value(self):
        detector = ContradictionDetector(storage=None)
        assert detector.STRONG_SIMILARITY_THRESHOLD == 0.85
        assert detector.STRONG_SIMILARITY_THRESHOLD > detector.SIMILARITY_THRESHOLD
