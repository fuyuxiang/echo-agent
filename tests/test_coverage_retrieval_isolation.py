"""跨 session 记忆隔离表征测试。

与已有 tests/test_memory_session_isolation.py 的区别：那里用桩函数当
visibility_fn；这里把真实的 MemoryStore.is_visible_in_session 接进
HybridRetriever，覆盖 store._visible_in_session 的两种 scope_policy
（legacy / session）在检索链路上的实际隔离行为。

只表征现有行为，不改源码。
"""

import pytest

from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier


def _make_store(tmp_path, scope_policy):
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy=scope_policy)


def _env_entry(key, content, session):
    return MemoryEntry(
        type=MemoryType.ENVIRONMENT,
        tier=MemoryTier.SEMANTIC,
        key=key,
        content=content,
        source_session=session,
    )


@pytest.mark.asyncio
async def test_session_policy_hides_other_session_env_memory(tmp_path):
    """scope_policy='session'：B 会话检索不应返回 A 会话的私有 ENV 记忆。"""
    store = _make_store(tmp_path, "session")
    a = _env_entry("proj", "alpha secret config", "s:A")
    b = _env_entry("proj", "beta visible config", "s:B")

    retriever = HybridRetriever(
        entries_fn=lambda: [a, b],
        visibility_fn=store.is_visible_in_session,
    )
    out = await retriever.retrieve("config", limit=5, memory_scope="s:B")
    contents = {e.content for e, _ in out}

    assert "alpha secret config" not in contents
    assert "beta visible config" in contents


@pytest.mark.asyncio
async def test_session_policy_global_tag_crosses_sessions(tmp_path):
    """scope_policy='session'：带 global 标签的记忆对任意会话可见。"""
    store = _make_store(tmp_path, "session")
    g = _env_entry("proj", "shared global config", "s:A")
    g.tags = ["global"]

    retriever = HybridRetriever(
        entries_fn=lambda: [g],
        visibility_fn=store.is_visible_in_session,
    )
    out = await retriever.retrieve("config", limit=5, memory_scope="s:B")
    contents = {e.content for e, _ in out}

    assert "shared global config" in contents


@pytest.mark.asyncio
async def test_legacy_policy_env_memory_visible_across_sessions(tmp_path):
    """scope_policy='legacy'（默认）：ENV 记忆不按 source_session 隔离，
    A 会话的 ENV 记忆对 B 会话仍可见——表征当前默认行为。"""
    store = _make_store(tmp_path, "legacy")
    a = _env_entry("proj", "alpha config", "s:A")

    retriever = HybridRetriever(
        entries_fn=lambda: [a],
        visibility_fn=store.is_visible_in_session,
    )
    out = await retriever.retrieve("config", limit=5, memory_scope="s:B")
    contents = {e.content for e, _ in out}

    assert "alpha config" in contents


@pytest.mark.asyncio
async def test_empty_session_key_skips_visibility_filter(tmp_path):
    """session_key 为空时，retrieve 不触发可见性过滤（短路）。"""
    store = _make_store(tmp_path, "session")
    a = _env_entry("proj", "alpha config", "s:A")

    retriever = HybridRetriever(
        entries_fn=lambda: [a],
        visibility_fn=store.is_visible_in_session,
    )
    out = await retriever.retrieve("config", limit=5, memory_scope="")
    contents = {e.content for e, _ in out}

    assert "alpha config" in contents
