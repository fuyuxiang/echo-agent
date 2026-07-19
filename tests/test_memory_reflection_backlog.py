"""②终审 backlog 三洞：reviewer replace 旁路、误导文案、被拦内容落行。"""
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


class TestReviewerReplaceGuard:
    @pytest.mark.asyncio
    async def test_replace_blocked_when_lower_priority(self, store):
        """后台 reviewer 的 replace 不得覆盖 user_stated 内容。"""
        from echo_agent.memory.reviewer import MemoryReviewer

        from echo_agent.memory.service import MemoryService

        e = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="用户明说住北京",
            source="user_stated", source_session="s1",
        ))
        reviewer = MemoryReviewer(
            provider=MagicMock(), service=MemoryService(store), session_key="s1",
        )
        result = await reviewer._execute({
            "action": "replace", "target": "user", "key": "home", "content": "推断住上海",
        })
        assert "Kept existing" in result
        entry = store.get(e.id)
        assert entry.content == "用户明说住北京"
        assert entry.source == "user_stated"
        # service 被拒仅拒绝,不再打 suspected_conflict tag(裁决留到重构层)。
        assert store.SUSPECTED_CONFLICT_TAG not in entry.tags

    @pytest.mark.asyncio
    async def test_replace_allowed_on_equal_priority(self, store):
        from echo_agent.memory.reviewer import MemoryReviewer
        from echo_agent.memory.service import MemoryService

        e = store.add(MemoryEntry(
            type=MemoryType.USER, key="job", content="旧推断",
            source="model_inferred", source_session="s1",
        ))
        reviewer = MemoryReviewer(
            provider=MagicMock(), service=MemoryService(store), session_key="s1",
        )
        result = await reviewer._execute({
            "action": "replace", "target": "user", "key": "job", "content": "新推断",
        })
        assert result.startswith("Updated")
        assert store.get(e.id).content == "新推断"


class TestAddHonestFeedback:
    @pytest.mark.asyncio
    async def test_add_reports_kept_existing_on_guard(self, store):
        """守卫拦截时 _add 不得谎称 saved。"""
        from echo_agent.agent.tools.base import ToolExecutionContext
        from echo_agent.agent.tools.memory import MemoryTool

        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:tea", content="用户明说喝绿茶",
            source="user_stated", source_session="s1",
        ))
        from echo_agent.memory.service import MemoryService

        tool = MemoryTool(MemoryService(store))
        # 同一会话作用域（s1）内写入，才会触发 _merge_locked 守卫。
        result = await tool.execute({
            "action": "add", "target": "user", "key": "pref:tea",
            "content": "推断喝咖啡",
        }, ctx=ToolExecutionContext(session_key="s1"))
        assert result.success
        assert "saved" not in result.output.lower()
        assert "Kept existing" in result.output

    @pytest.mark.asyncio
    async def test_add_normal_still_says_saved(self, store):
        from echo_agent.agent.tools.base import ToolExecutionContext
        from echo_agent.agent.tools.memory import MemoryTool
        from echo_agent.memory.service import MemoryService

        tool = MemoryTool(MemoryService(store))
        result = await tool.execute({
            "action": "add", "target": "user", "key": "fresh", "content": "全新条目",
        }, ctx=ToolExecutionContext(session_key="s1"))
        assert "Memory saved" in result.output


class TestGuardSideEffects:
    def test_guard_does_not_refresh_updated_at(self, store):
        e = store.add(MemoryEntry(
            type=MemoryType.USER, key="k", content="高优先级内容",
            source="user_stated", source_session="s1",
        ))
        original_ts = store.get(e.id).updated_at
        store.add(MemoryEntry(
            type=MemoryType.USER, key="k", content="低优先级来犯",
            source="model_inferred", source_session="s1",
        ))
        assert store.get(e.id).updated_at == original_ts

    def test_guard_records_blocked_contradiction(self, store, tmp_path):
        """被拦内容须落 Contradiction 行,且 memory_id_b 为真实落库条目 id
        (R4:不再是 blocked:<source> 占位),store.get 可取以供 reflection 裁决。"""
        calls = []

        class FakeStorage:
            async def execute_sql(self, sql, params=()):
                calls.append((sql, params))

            async def store_memory(self, *a, **k):
                pass

        s = MemoryStore(tmp_path / "m2", storage=FakeStorage())
        s.add(MemoryEntry(
            type=MemoryType.USER, key="k", content="高优先级",
            source="user_stated", source_session="s1",
        ))
        s.add(MemoryEntry(
            type=MemoryType.USER, key="k", content="被拦的低优先级内容",
            source="model_inferred", source_session="s1",
        ))
        contradiction_inserts = [
            (sql, p) for sql, p in calls if "memory_contradictions" in sql
        ]
        assert len(contradiction_inserts) == 1
        _, params = contradiction_inserts[0]
        # memory_id_b 是真实落库条目 id,不再是 blocked:<source> 占位。
        assert not params[2].startswith("blocked:")
        landed = s.get(params[2])
        assert landed is not None and landed.content == "被拦的低优先级内容"
        assert "被拦的低优先级内容" in params[3]  # description 含被拦文本
        # 被拒仍不改 active:胜者高优先级未动、pending 落库条目不进召回(ARCHIVAL)。
        active = [e for e in s._entries.values()
                  if e.key == "k" and not e.is_superseded
                  and e.tier != MemoryTier.ARCHIVAL]
        assert len(active) == 1 and active[0].content == "高优先级"
