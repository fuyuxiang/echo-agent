import pytest
from echo_agent.memory.tiers import SemanticManager, Episode
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryType


def _make_mgr(tmp_path, allow_env_writes=False):
    """构造走 MemoryService 八步写序的 SemanticManager 与其底层 store。"""
    store = MemoryStore(memory_dir=tmp_path / "mem")
    service = MemoryService(store, allow_env_writes=allow_env_writes)
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
async def test_promote_environment_fact_denied_by_default(tmp_path):
    """默认 allow_env_writes=False 时 consolidation 写全局 ENV 被 ENV 门禁拒绝。

    consolidation 提炼的 fact 来自 LLM 对话(不可信),模型可显式输出
    type=environment 绕过 allow_model_environment_writes 写全局 ENV。
    consolidation 纳入 ENV 门禁后,默认配置下该 ENV 写被拒(promoted 为空、库中无该条目)。
    """
    mgr, store = _make_mgr(tmp_path, allow_env_writes=False)
    episode = _make_episode("telegram:room1")
    facts = [{"type": "environment", "key": "tz", "content": "UTC+8"}]
    promoted = await mgr.promote_from_episodic(episode, facts)
    assert promoted == []  # ENV 写被门禁拒绝,不晋升
    assert store.find_by_key("tz", MemoryType.ENVIRONMENT) is None


@pytest.mark.asyncio
async def test_promote_environment_fact_allowed_when_enabled(tmp_path):
    """管理员显式开 allow_model_environment_writes 后 consolidation 才可写全局 ENV。"""
    mgr, _ = _make_mgr(tmp_path, allow_env_writes=True)
    episode = _make_episode("telegram:room1")
    facts = [{"type": "environment", "key": "tz", "content": "UTC+8"}]
    promoted = await mgr.promote_from_episodic(episode, facts)
    # 开关打开:ENV 事实写成功且保持全局可见(source_session 空)
    assert promoted, "开启 allow_env_writes 后 ENV 事实应被成功晋升"
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
