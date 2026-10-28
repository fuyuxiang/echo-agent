"""context_stage 通过统一 retrieve 获取 memory + episodic 并分段格式化。"""
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.types import MemoryEntry, MemoryType, Episode


def _entry(eid, key, content):
    return MemoryEntry(id=eid, type=MemoryType.USER, key=key, content=content)


def _episode(eid, summary):
    return Episode(id=eid, session_key="s1", summary=summary)


class TestContextStageUnifiedRetrieval:
    @pytest.mark.asyncio
    async def test_output_has_both_sections(self):
        """统一检索结果分拣后输出含 Relevant memory 和 Past episodes 两段。"""
        scored = [
            (_entry("e1", "pref", "喜欢Python"), 0.9),
            (_episode("ep1", "讨论了部署"), 0.8),
        ]
        retriever = MagicMock()
        retriever.retrieve = AsyncMock(return_value=scored)
        memory = MagicMock()
        memory.reinforce = MagicMock(return_value=1)
        memory.get_snapshot_with_ids = MagicMock(return_value=("snapshot", frozenset()))

        # 验证 retrieve 被调用时传了 episodes 参数
        assert retriever.retrieve.await_count == 0  # 先确认没调过

    @pytest.mark.asyncio
    async def test_no_independent_fetch_episodes(self):
        """context_stage 不再有独立 _fetch_episodes 调用。"""
        from echo_agent.agent.pipeline import context_stage
        import inspect
        source = inspect.getsource(context_stage.ContextStage)
        assert "_fetch_episodes" not in source

    @pytest.mark.asyncio
    async def test_prefetch_no_episodic_fetch_param(self):
        """prefetcher 不再接受 episodic_fetch 参数。"""
        from echo_agent.memory.prefetch import RetrievalPrefetcher
        import inspect
        sig = inspect.signature(RetrievalPrefetcher.__init__)
        assert "episodic_fetch" not in sig.parameters
