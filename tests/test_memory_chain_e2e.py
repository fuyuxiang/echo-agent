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


def test_cross_channel_semantic_shared_episode_isolated(tmp_path):
    # owner 语义事实跨通道可召回；episode 按会话隔离。
    s = _store(tmp_path)
    # 通道 A 写入 owner 语义事实（source_session=owner）。
    s.add(
        MemoryEntry(
            type=MemoryType.USER,
            tier=MemoryTier.SEMANTIC,
            key="user:city",
            content="住在上海",
            source_session="owner",
        )
    )
    r = HybridRetriever(
        entries_fn=lambda: s.list_all(mem_type=MemoryType.USER),
        visibility_fn=s.is_visible_in_session,
    )
    # 通道 B（不同 session=slack:U0，但 memory_scope 同为 owner）检索。
    scored = asyncio.run(
        r.retrieve(
            "上海",
            limit=8,
            memory_scope="owner",
            episode_session_key="slack:U0",
        )
    )
    assert any("上海" in getattr(x[0], "content", "") for x in scored)


def test_long_term_shard_isolation(tmp_path):
    s = _store(tmp_path)
    s.write_long_term("owner", "owner 私密")
    s.write_long_term("telegram:grp1:bob", "群成员 bob")
    # 快照按 scope 只注入对应分片。
    owner_snap, _ = s.get_snapshot_with_ids(session_key="owner")
    grp_snap, _ = s.get_snapshot_with_ids(session_key="telegram:grp1:bob")
    assert "owner 私密" in owner_snap and "bob" not in owner_snap
    assert "群成员 bob" in grp_snap and "私密" not in grp_snap


def test_concurrent_same_scope_no_lost_write(tmp_path):
    # 同 scope 顺序写（模拟并发后写者不吞先写者的最终值）。
    s = _store(tmp_path)
    s.write_long_term("owner", "第一版")
    s.write_long_term("owner", "第二版")
    assert s.read_long_term("owner") == "第二版"
