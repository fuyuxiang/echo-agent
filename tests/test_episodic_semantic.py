"""episodic 语义化：嵌入入库、语义检索、LIKE 降级、孤儿分表、stale 回填。"""
from pathlib import Path

import pytest
import pytest_asyncio

from echo_agent.memory.tiers import EpisodicManager
from echo_agent.memory.vectors import VectorIndex
from echo_agent.storage.sqlite import SQLiteBackend

MODEL = "fastembed:test"


def _embed_factory(mapping):
    """确定性伪嵌入：按关键词映射到固定向量，模拟语义相近。"""
    async def _embed(text: str) -> list[float]:
        for kw, vec in mapping.items():
            if kw in text:
                return vec
        return [0.0, 0.0, 0.0, 1.0]
    return _embed


@pytest_asyncio.fixture
async def storage(tmp_path: Path) -> SQLiteBackend:
    backend = SQLiteBackend(tmp_path / "test.db")
    await backend.initialize()
    yield backend
    await backend.close()


@pytest_asyncio.fixture
async def episodic(storage) -> EpisodicManager:
    index = VectorIndex(storage, dimensions=4, model_id=MODEL)
    await index.initialize()
    mgr = EpisodicManager(storage)
    embed = _embed_factory({
        "部署": [1.0, 0.0, 0.0, 0.0],
        "上线": [0.9, 0.1, 0.0, 0.0],   # 与"部署"语义相近
        "宠物": [0.0, 1.0, 0.0, 0.0],
    })
    mgr.attach_embedding(embed, index)
    return mgr


@pytest.mark.asyncio
async def test_create_episode_stores_prefixed_vector(episodic, storage):
    ep = await episodic.create_episode("s1", [], "讨论了项目部署方案")
    rows = await storage.load_vectors_all()
    assert any(r["source_id"] == f"ep:{ep.id}" for r in rows)


@pytest.mark.asyncio
async def test_semantic_search_finds_synonym(episodic):
    """LIKE 匹配不到的同义查询能语义命中。"""
    ep = await episodic.create_episode("s1", [], "讨论了项目部署方案")
    await episodic.create_episode("s1", [], "聊了宠物猫的名字")
    results = await episodic.search_episodes("上线", session_key="s1", limit=3)
    assert results and results[0].id == ep.id


@pytest.mark.asyncio
async def test_semantic_search_respects_session_filter(episodic):
    await episodic.create_episode("s1", [], "讨论了项目部署方案")
    results = await episodic.search_episodes("上线", session_key="other", limit=3)
    assert results == []


@pytest.mark.asyncio
async def test_fallback_to_like_without_embedding(storage):
    mgr = EpisodicManager(storage)  # 未 attach → LIKE 路径
    ep = await mgr.create_episode("s1", [], "讨论了项目部署方案")
    results = await mgr.search_episodes("部署", session_key="s1", limit=3)
    assert results and results[0].id == ep.id


@pytest.mark.asyncio
async def test_scan_orphan_vectors_checks_episodes_table(storage, episodic, tmp_path):
    """ep: 向量对照 episodes 表而非 entries；活 episode 向量不被误删。"""
    from echo_agent.memory.store import MemoryStore

    ep = await episodic.create_episode("s1", [], "讨论了项目部署方案")
    import numpy as np
    dead = np.array([1.0, 0, 0, 0], dtype=np.float32).tobytes()
    await storage.store_vector("v_dead_ep", "ep:gone_episode", dead, {}, model=MODEL, dim=4)

    store = MemoryStore(tmp_path / "memory", storage=storage)
    removed = await store.scan_orphan_vectors()
    rows = await storage.load_vectors_all()
    ids = {r["source_id"] for r in rows}
    assert f"ep:{ep.id}" in ids          # 活 episode 保留
    assert "ep:gone_episode" not in ids  # 死 episode 向量删除
    assert removed >= 1


@pytest.mark.asyncio
async def test_requeue_stale_reembeds_episode(storage):
    """模型切换后 ep: 向量按 summary 重嵌入。"""
    old_index = VectorIndex(storage, dimensions=4, model_id="fastembed:old")
    await old_index.initialize()
    mgr_old = EpisodicManager(storage)
    mgr_old.attach_embedding(_embed_factory({"部署": [1.0, 0, 0, 0]}), old_index)
    ep = await mgr_old.create_episode("s1", [], "讨论了项目部署方案")

    new_index = VectorIndex(storage, dimensions=4, model_id=MODEL)
    await new_index.initialize()
    assert f"ep:{ep.id}" in new_index.stale_source_ids

    mgr_new = EpisodicManager(storage)
    mgr_new.attach_embedding(_embed_factory({"部署": [0.5, 0.5, 0, 0]}), new_index)
    n = await mgr_new.requeue_stale(new_index.stale_source_ids)
    assert n == 1
    rows = await storage.load_vectors_all()
    ep_rows = [r for r in rows if r["source_id"] == f"ep:{ep.id}"]
    assert len(ep_rows) == 1 and ep_rows[0]["model"] == MODEL
