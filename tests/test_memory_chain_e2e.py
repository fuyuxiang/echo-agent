"""Phase 2 检索链路端到端测试。

用真实 MemoryStore + HybridRetriever（不桩接核心逻辑），验证 spec 关键场景：
- 跨通道语义共享 + episode 按会话隔离（检索器双键：memory_scope 管可见性，
  episode_session_key 管 episode 候选）。
- 长期记忆（MEMORY.md）按 scope 分片隔离。
- 同 scope 顺序写后写者不丢失最终值。

说明（对齐真实签名，保持测试意图不变）：
- MemoryStore 构造为 ``memory_dir: Path``（非 data_dir=str）。
- ``add`` 接收一个 ``MemoryEntry``（非散列关键字参数）。
- scope_policy 用 ``session`` 才能让 memory_scope 真正参与可见性过滤；
  legacy 策略下 USER 记忆对所有会话可见，双键拆分会退化为 no-op。
- 未接入 embed_fn / 向量索引，检索走 BM25；CJK 按单字+二元组分词，故用
  与语义事实词面重叠的查询词来召回语义层记忆（"语义"指语义层记忆而非向量相似）。
"""
from __future__ import annotations

import asyncio

from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryTier, MemoryType


def _store(tmp_path):
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")


def test_cross_channel_semantic_shared_and_isolated(tmp_path):
    # owner 语义事实跨通道可召回(正向);非 owner 事实在 owner 域不可见(负向)。
    s = _store(tmp_path)
    s.add(MemoryEntry(
        type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
        key="user:city", content="住在上海", source_session="owner",
    ))
    s.add(MemoryEntry(
        type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
        key="user:city", content="住在北京", source_session="telegram:bob",
    ))
    r = HybridRetriever(
        entries_fn=lambda: s.list_all(mem_type=MemoryType.USER),
        visibility_fn=s.is_visible_in_session,
    )
    scored = asyncio.run(r.retrieve(
        "上海 北京", limit=8, memory_scope="owner", episode_session_key="slack:U0",
    ))
    contents = [getattr(x[0], "content", "") for x in scored]
    # 正向:owner 事实跨通道(slack session)可召回
    assert any("上海" in c for c in contents)
    # 负向:非 owner(telegram:bob)事实在 owner 域不可见——证明 scope 真隔离
    assert not any("北京" in c for c in contents)


def test_episode_isolated_by_session(tmp_path):
    # episode 按 episode_session_key 隔离:注入模拟真实"按 session 精确匹配"的
    # episode_search_fn(与 tiers.py 的 ep.session_key == session_key 一致),
    # 断言只有当前会话的 episode 进入候选。
    from echo_agent.memory.types import Episode

    all_eps = [
        Episode(id="epA", session_key="telegram:alice", summary="alice 讨论了部署"),
        Episode(id="epB", session_key="slack:bob", summary="bob 讨论了发布"),
    ]

    async def episode_search(query, session_key, limit):
        return [e for e in all_eps if e.session_key == session_key]

    s = _store(tmp_path)
    r = HybridRetriever(
        entries_fn=lambda: s.list_all(mem_type=MemoryType.USER),
        visibility_fn=s.is_visible_in_session,
        episode_search_fn=episode_search,
    )
    scored = asyncio.run(r.retrieve(
        "讨论", limit=8, memory_scope="owner", episode_session_key="telegram:alice",
    ))
    # 检索结果里 episode 命中返回原始 Episode 对象(其正文在 summary 字段),
    # 记忆条目则用 content 字段,故两者都取到才不漏检。
    summaries = [
        getattr(x[0], "summary", "") or getattr(x[0], "content", "") for x in scored
    ]
    # 只有 alice 会话的 episode 入候选,bob 的不出现
    assert any("alice 讨论了部署" in sm for sm in summaries)
    assert not any("bob 讨论了发布" in sm for sm in summaries)


def test_long_term_shard_isolation(tmp_path):
    s = _store(tmp_path)
    s.write_long_term("owner", "owner 私密")
    s.write_long_term("telegram:grp1:bob", "群成员 bob")

    def _shard(scope):
        p = s._long_term_path(scope)
        return p.read_text(encoding="utf-8") if p.exists() else ""

    # R3:MD 按 scope 分片隔离(落盘分片文件),但不再注入快照/prompt。
    assert "owner 私密" in _shard("owner")
    assert "bob" not in _shard("owner")
    assert "群成员 bob" in _shard("telegram:grp1:bob")
    assert "私密" not in _shard("telegram:grp1:bob")
    # 快照不含任一分片的 MD 文本。
    owner_snap, _ = s.get_snapshot_with_ids(session_key="owner")
    grp_snap, _ = s.get_snapshot_with_ids(session_key="telegram:grp1:bob")
    assert "## Long-term Memory" not in owner_snap
    assert "owner 私密" not in owner_snap
    assert "群成员 bob" not in grp_snap


def test_concurrent_same_scope_no_lost_write(tmp_path):
    # 同 scope 顺序写（模拟并发后写者不吞先写者的最终值）。
    s = _store(tmp_path)
    s.write_long_term("owner", "第一版")
    s.write_long_term("owner", "第二版")
    _p = s._long_term_path("owner")
    assert _p.read_text(encoding="utf-8") == "第二版"
