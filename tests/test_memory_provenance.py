"""记忆来源分级（provenance）：字段序列化、优先级函数、打标、裁决。"""
from echo_agent.memory.types import MemoryEntry, source_priority


class TestSourceField:
    def test_default_is_legacy(self):
        assert MemoryEntry().source == "legacy"

    def test_serialization_roundtrip(self):
        e = MemoryEntry(key="k", content="c", source="user_stated")
        restored = MemoryEntry.from_dict(e.to_dict())
        assert restored.source == "user_stated"

    def test_from_dict_missing_source_falls_to_legacy(self):
        """存量 JSON 无 source 字段 → legacy。"""
        e = MemoryEntry(key="k", content="c")
        data = e.to_dict()
        del data["source"]
        assert MemoryEntry.from_dict(data).source == "legacy"


class TestSourcePriority:
    def test_ordering(self):
        assert (
            source_priority("user_stated")
            > source_priority("consolidated")
            > source_priority("model_inferred")
            > source_priority("legacy")
        )

    def test_exact_values(self):
        assert source_priority("user_stated") == 3
        assert source_priority("consolidated") == 2
        assert source_priority("model_inferred") == 1
        assert source_priority("legacy") == 0

    def test_unknown_word_is_zero(self):
        assert source_priority("some_future_source") == 0
        assert source_priority("") == 0
