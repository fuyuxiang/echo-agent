import pytest
from echo_agent.memory.tiers import SemanticManager, Episode
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryType


def _make_mgr(tmp_path):
    """构造走 MemoryService 八步写序的 SemanticManager 与其底层 store。"""
    store = MemoryStore(memory_dir=tmp_path / "mem")
    service = MemoryService(store)
    return SemanticManager(service), store


def _make_episode(session_key: str):
    return Episode(id="ep1", session_key=session_key, summary="s")


@pytest.mark.asyncio
async def test_promote_user_fact_carries_source_session(tmp_path):
    mgr, _ = _make_mgr(tmp_path)
    episode = _make_episode("telegram:room1")
    facts = [{"type": "user", "key": "pref", "content": "likes dark mode"}]
    promoted = await mgr.promote_from_episodic(episode, facts)
    assert promoted[0].source_session == "telegram:room1"


@pytest.mark.asyncio
async def test_promote_environment_fact_no_source_session(tmp_path):
    mgr, _ = _make_mgr(tmp_path)
    episode = _make_episode("telegram:room1")
    facts = [{"type": "environment", "key": "tz", "content": "UTC+8"}]
    promoted = await mgr.promote_from_episodic(episode, facts)
    # ENV 事实保持全局可见(source_session 空),不落当前 scope
    assert promoted[0].source_session == ""


@pytest.mark.asyncio
async def test_promotion_defaults_to_user_scope_not_environment(tmp_path):
    """fact 不指定 type 时默认落 USER + 当前 memory_scope,而非全局可见的 ENVIRONMENT。"""
    mgr, _ = _make_mgr(tmp_path)
    episode = _make_episode("telegram:room1")
    facts = [{"key": "pref", "content": "no explicit type"}]  # 不带 type
    promoted = await mgr.promote_from_episodic(episode, facts)
    assert promoted, "默认类型的事实应被成功晋升"
    assert promoted[0].type == MemoryType.USER
    assert promoted[0].source_session == "telegram:room1"
    assert promoted[0].source == "consolidated"
