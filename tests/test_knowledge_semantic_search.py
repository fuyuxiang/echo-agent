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

