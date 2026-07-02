"""反思引擎：归纳提炼（distill）。"""
import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.reflection import ReflectionEngine
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


def _llm_returning(tool_name: str, args: dict):
    resp = MagicMock()
    tc = MagicMock()
    tc.name = tool_name
    tc.arguments = json.dumps(args)
    resp.tool_calls = [tc]
    return AsyncMock(return_value=resp)


def _add_prefixed(store, n, prefix="pref"):
    entries = []
    for i in range(n):
        entries.append(store.add(MemoryEntry(
            type=MemoryType.USER, key=f"{prefix}:item{i}",
            content=f"喜欢事物{i}", source="model_inferred",
        )))
    return entries


class TestDistill:
    @pytest.mark.asyncio
    async def test_distills_group_of_three(self, store):
        _add_prefixed(store, 3)
        llm = _llm_returning("save_distilled", {
            "distill": True, "key": "pref:general",
            "content": "用户对新事物普遍持开放喜好", "importance": 0.7,
        })
        engine = ReflectionEngine(store, llm_call=llm)
        created = await engine.distill()
        assert created == 1
        general = store.find_by_key("pref:general")
        assert general is not None
        assert general.source == "consolidated"
        assert "distilled" in general.tags
        # 只增不删：原条目全在
        assert store.find_by_key("pref:item0") is not None

    @pytest.mark.asyncio
    async def test_skips_group_below_threshold(self, store):
        _add_prefixed(store, 2)  # 少于 3 条
        llm = AsyncMock()
        engine = ReflectionEngine(store, llm_call=llm)
        assert await engine.distill() == 0
        llm.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_skips_group_with_existing_general(self, store):
        _add_prefixed(store, 3)
        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:general",
            content="已有规律", source="consolidated",
        ))
        llm = AsyncMock()
        engine = ReflectionEngine(store, llm_call=llm)
        assert await engine.distill() == 0
        llm.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_llm_declines_distill(self, store):
        _add_prefixed(store, 3)
        llm = _llm_returning("save_distilled", {"distill": False})
        engine = ReflectionEngine(store, llm_call=llm)
        assert await engine.distill() == 0

    @pytest.mark.asyncio
    async def test_max_groups_cap(self, store):
        _add_prefixed(store, 3, prefix="a")
        _add_prefixed(store, 3, prefix="b")
        _add_prefixed(store, 3, prefix="c")
        llm = _llm_returning("save_distilled", {"distill": False})
        engine = ReflectionEngine(store, llm_call=llm)
        await engine.distill(max_groups=2)
        assert llm.await_count == 2

    @pytest.mark.asyncio
    async def test_llm_failure_returns_zero(self, store):
        _add_prefixed(store, 3)
        engine = ReflectionEngine(store, llm_call=AsyncMock(side_effect=RuntimeError("boom")))
        assert await engine.distill() == 0  # 不抛异常


def test_config_reflection_enabled_default_true():
    from echo_agent.config.schema import MemoryConfig
    assert MemoryConfig().reflection_enabled is True
