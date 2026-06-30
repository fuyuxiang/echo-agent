from pathlib import Path

import pytest

from echo_agent.knowledge.index import KnowledgeIndex


def _mk_index(tmp_path: Path) -> KnowledgeIndex:
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nthe quick brown fox", encoding="utf-8")
    (docs / "b.md").write_text("# Beta\nlazy dog sleeps", encoding="utf-8")
    return KnowledgeIndex(
        workspace=tmp_path, docs_dir="docs", index_path="idx.json",
        allowed_extensions=[".md"],
    )


def test_v2_index_has_content_hash(tmp_path: Path):
    idx = _mk_index(tmp_path)
    idx.rebuild()
    import json
    data = json.loads((tmp_path / "idx.json").read_text(encoding="utf-8"))
    assert data["format"] == "echo-agent-knowledge-v2"
    assert all("content_hash" in c for c in data["chunks"])


def test_keyword_scores_shared(tmp_path: Path):
    idx = _mk_index(tmp_path)
    idx.rebuild()
    results = idx.search("quick fox", limit=5)
    assert results and "Alpha" in results[0].title


def test_v1_index_auto_upgrades_to_v2(tmp_path: Path):
    import json

    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nthe quick brown fox", encoding="utf-8")
    legacy = {
        "format": "echo-agent-knowledge-v1",
        "generated_at": "2020-01-01T00:00:00",
        "docs_dir": str(docs),
        "chunks": [
            {
                "id": "docs/a.md#0",
                "path": "docs/a.md",
                "title": "Alpha",
                "text": "the quick brown fox",
                "terms": {"quick": 1, "fox": 1},
                "metadata": {},
                "mtime": 0.0,
            }
        ],
    }
    index_path = tmp_path / "idx.json"
    index_path.write_text(json.dumps(legacy, ensure_ascii=False), encoding="utf-8")

    idx = KnowledgeIndex(
        workspace=tmp_path, docs_dir="docs", index_path="idx.json",
        allowed_extensions=[".md"],
    )
    idx.ensure_ready(auto_index=True)

    data = json.loads(index_path.read_text(encoding="utf-8"))
    assert data["format"] == "echo-agent-knowledge-v2"
    assert data["chunks"]
    assert all("content_hash" in c for c in data["chunks"])


async def _fake_embed(text: str) -> list[float]:
    # 极简确定性嵌入:按关键词命中维度,3 维
    v = [0.0, 0.0, 0.0]
    if "fox" in text or "quick" in text:
        v[0] = 1.0
    if "dog" in text or "lazy" in text:
        v[1] = 1.0
    if not any(v):
        v[2] = 1.0
    return v


@pytest.mark.asyncio
async def test_search_async_degrades_to_sync_when_no_embed(tmp_path):
    from echo_agent.knowledge.index import KnowledgeIndex
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nthe quick brown fox", encoding="utf-8")
    (docs / "b.md").write_text("# Beta\nlazy dog sleeps", encoding="utf-8")
    idx = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                         allowed_extensions=[".md"])
    idx.rebuild()
    # 未 attach embedding → search_async 必须等价同步 search
    sync = idx.search("quick fox", limit=5)
    asyncd = await idx.search_async("quick fox", limit=5)
    assert [r.chunk_id for r in sync] == [r.chunk_id for r in asyncd]


@pytest.mark.asyncio
async def test_rebuild_async_incremental_reuse(tmp_path):
    from echo_agent.knowledge.index import KnowledgeIndex
    pytest.importorskip("faiss")
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nquick fox", encoding="utf-8")
    idx = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                         allowed_extensions=[".md"])
    calls = {"n": 0}

    async def counting_embed(text: str):
        calls["n"] += 1
        return await _fake_embed(text)

    idx.attach_embedding(counting_embed, dimensions=3)
    await idx.rebuild_async()
    first = calls["n"]
    assert first >= 1
    # 不改文件再 rebuild_async → 复用,embed 调用不增
    await idx.rebuild_async()
    assert calls["n"] == first


@pytest.mark.asyncio
async def test_rebuild_async_recomputes_on_content_change(tmp_path):
    from echo_agent.knowledge.index import KnowledgeIndex
    pytest.importorskip("faiss")
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nquick fox", encoding="utf-8")
    idx = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                         allowed_extensions=[".md"])
    calls = {"n": 0}

    async def counting_embed(text: str):
        calls["n"] += 1
        return await _fake_embed(text)

    idx.attach_embedding(counting_embed, dimensions=3)
    await idx.rebuild_async()
    first = calls["n"]
    # 改内容但保持单块(分块数不变 → chunk id 不变,content_hash 变) → 必须重算
    (docs / "a.md").write_text("# Alpha\nslow turtle", encoding="utf-8")
    await idx.rebuild_async()
    assert calls["n"] > first


@pytest.mark.asyncio
async def test_search_async_vector_recall(tmp_path):
    from echo_agent.knowledge.index import KnowledgeIndex
    pytest.importorskip("faiss")
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nquick fox", encoding="utf-8")
    (docs / "b.md").write_text("# Beta\nlazy dog", encoding="utf-8")
    idx = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                         allowed_extensions=[".md"])
    idx.attach_embedding(_fake_embed, dimensions=3)
    await idx.rebuild_async()
    res = await idx.search_async("fox", limit=2)
    assert res and "Alpha" in res[0].title


@pytest.mark.asyncio
async def test_attach_then_background_backfill_flag(tmp_path):
    from echo_agent.knowledge.index import KnowledgeIndex
    pytest.importorskip("faiss")
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nquick fox", encoding="utf-8")
    idx = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                         allowed_extensions=[".md"])
    idx.rebuild()  # 纯关键词索引先就绪
    idx.attach_embedding(_fake_embed, dimensions=3)
    # attach 不算嵌入 → 仍需补建
    assert idx.needs_vector_backfill() is True
    await idx.rebuild_async()
    assert idx.needs_vector_backfill() is False


@pytest.mark.asyncio
async def test_backfill_detects_content_change_across_restart(tmp_path):
    """重启场景:文档内容变(分块数不变 → chunk id 不变),启动期 ensure_ready 先把
    文本索引 rebuild 成新内容,随后 attach_embedding 从 sidecar 载入旧向量。
    needs_vector_backfill 必须靠 sidecar 自记录的 content_hash 发现陈旧,否则语义
    检索会一直吃旧 embedding。"""
    from echo_agent.knowledge.index import KnowledgeIndex
    pytest.importorskip("faiss")
    docs = tmp_path / "docs"
    docs.mkdir()
    (docs / "a.md").write_text("# Alpha\nquick fox", encoding="utf-8")

    # 第一次运行:索引 + 向量补建,sidecar 落 fox 向量
    idx1 = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                          allowed_extensions=[".md"])
    idx1.attach_embedding(_fake_embed, dimensions=3)
    await idx1.rebuild_async()
    assert idx1.needs_vector_backfill() is False

    # 进程关闭期间内容被改(单块,chunk id 不变,content_hash 变)
    (docs / "a.md").write_text("# Alpha\nlazy dog", encoding="utf-8")
    # 模拟重启启动期:ensure_ready 先同步 rebuild 文本索引(idx.json 变新 hash)
    idx2 = KnowledgeIndex(workspace=tmp_path, docs_dir="docs", index_path="idx.json",
                          allowed_extensions=[".md"])
    idx2.rebuild()  # ensure_ready 在 stale 时做的事:刷新文本索引,不碰向量
    # 随后挂载 embedding,从 sidecar 载入旧(fox)向量
    idx2.attach_embedding(_fake_embed, dimensions=3)

    # 旧 sidecar 向量对应的是 fox,当前内容是 dog → 必须判定需要补建
    assert idx2.needs_vector_backfill() is True
    await idx2.rebuild_async()
    # 补建后,语义检索应召回新内容(dog),而非旧的 fox
    res = await idx2.search_async("dog", limit=1)
    assert res and "dog" in res[0].text.lower()


